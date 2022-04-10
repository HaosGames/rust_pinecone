use crate::coordinates::Coordinates;
use crate::frames::{
    Frame, SnekBootstrap, SnekBootstrapAck, SnekSetup, SnekSetupAck, SnekTeardown,
};
use crate::snek::{SnekPath, SnekPathIndex, SnekRouted};
use crate::tree::TreeRouted;
use crate::wait_timer::WaitTimer;
use crate::{PineconeCodec, Root, TreeAnnouncement};
use log::{debug, info, trace};
use rand::{thread_rng, Rng};
use std::collections::HashMap;
use std::ops::Add;
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tokio::sync::mpsc::{Receiver, Sender};
use tokio::sync::{Mutex, RwLock};
use tokio::task::JoinHandle;
use tokio::time::error::Elapsed;
use tokio::time::{Instant, interval, interval_at, sleep};
use tokio::{join, time};
use crate::connection::{DownloadConnection, UploadConnection};

pub type Port = u64;
pub type SequenceNumber = u64;
pub type SnekPathId = u64;
pub type VerificationKey = [u8; 32];

pub(crate) const SNEK_EXPIRY_PERIOD: Duration = Duration::from_secs(60 * 60);

pub(crate) const ANNOUNCEMENT_TIMEOUT: Duration = Duration::from_secs(45 * 60); //45 min
pub(crate) const ANNOUNCEMENT_INTERVAL: Duration = Duration::from_secs(30 * 60); // 30 min
pub(crate) const REPARENT_WAIT_TIME: Duration = Duration::from_secs(1); //   1 sec
pub(crate) const MAINTAIN_SNEK_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Clone)]
pub struct Router {
    public_key: Arc<VerificationKey>,
    running: Arc<RwLock<bool>>,

    pub(crate) upload: Arc<Mutex<Receiver<Frame>>>,
    pub(crate) download: Arc<Sender<Frame>>,
    upload_connections: Arc<RwLock<HashMap<VerificationKey, Mutex<UploadConnection>>>>,
    download_connections: Arc<RwLock<HashMap<VerificationKey, Mutex<DownloadConnection>>>>,
    ports: Arc<RwLock<HashMap<VerificationKey, Port>>>,

    parent: Arc<RwLock<VerificationKey>>,
    announcements: Arc<RwLock<HashMap<VerificationKey, TreeAnnouncement>>>,
    sequence: Arc<RwLock<SequenceNumber>>,
    ordering: Arc<RwLock<SequenceNumber>>,
    announcement_timer: Arc<RwLock<WaitTimer>>,
    reparent_timer: Arc<RwLock<Option<WaitTimer>>>,

    ascending_path: Arc<RwLock<Option<SnekPathIndex>>>,
    descending_path: Arc<RwLock<Option<SnekPathIndex>>>,
    paths: Arc<RwLock<HashMap<SnekPathIndex, SnekPath>>>,
    candidate: Arc<RwLock<Option<SnekPath>>>,
}
impl Router {
    pub fn new(key: VerificationKey, download: Sender<Frame>, upload: Receiver<Frame>) -> Self {
        Self {
            upload: Arc::new(Mutex::new(upload)),
            public_key: Arc::new(key),
            download: Arc::new(download),
            upload_connections: Arc::new(Default::default()),
            download_connections: Arc::new(Default::default()),
            ports: Default::default(),
            parent: Arc::new(RwLock::new(key)),
            announcements: Default::default(),
            sequence: Arc::new(RwLock::new(0)),
            ordering: Arc::new(RwLock::new(0)),
            announcement_timer: Arc::new(RwLock::new(WaitTimer::new(ANNOUNCEMENT_INTERVAL))),
            reparent_timer: Arc::new(RwLock::new(None)),
            ascending_path: Arc::new(RwLock::new(None)),
            descending_path: Arc::new(RwLock::new(None)),
            paths: Arc::new(Default::default()),
            candidate: Arc::new(RwLock::new(None)),
            running: Arc::new(RwLock::new(false)),
        }
    }
    pub async fn start(&self) -> JoinHandle<Self> {
        let mut running = self.running.write().await;
        *running = true;
        drop(running);
        let router = self.clone();
        tokio::spawn(async move {
            let mut ticker = interval_at(Instant::now().add(ANNOUNCEMENT_INTERVAL), ANNOUNCEMENT_INTERVAL);
            loop {
                ticker.tick().await;
                let running = router.running.read().await;
                if *running == false {
                    info!("Stopped router");
                    break;
                }
                drop(running);

                router.maintain_tree().await;
            }
        });

        let router = self.clone();
        tokio::spawn(async move {
            let mut ticker = interval_at(Instant::now().add(MAINTAIN_SNEK_INTERVAL), MAINTAIN_SNEK_INTERVAL);
            loop {
                ticker.tick().await;
                let running = router.running.read().await;
                if *running == false {
                    info!("Stopped router");
                    break;
                }
                drop(running);

                router.maintain_snek().await;
            }
        });


        let router = self.clone();
        tokio::spawn(async move {
            let mut upload = router.upload.lock().await;
            loop {
                let running = router.running.read().await;
                if *running == false {
                    info!("Stopped router");
                    break;
                }
                drop(running);
                if let Some(frame) = upload.recv().await {
                    router.handle_frame(frame, *router.public_key).await;
                } else {
                    debug!("Local event channel closed. Stopping router");
                    break;
                }
            }
            drop(upload);
            router
        })
    }
    pub async fn stop(&self) {
        *self.running.write().await = false;
    }
    pub async fn add_peer(&self, peer: VerificationKey, upload: UploadConnection, download: DownloadConnection) {
        let mut upload_connections = self.upload_connections.write().await;
        let mut download_connections = self.download_connections.write().await;
        if download_connections.contains_key(&peer) {
            info!("Couldn't add {:?} because it already exists", peer);
            return;
        }
        if upload_connections.contains_key(&peer) {
            info!("Couldn't add {:?} because it already exists", peer);
            return;
        }
        upload_connections.insert(peer, Mutex::new(upload));
        download_connections.insert(peer, Mutex::new(download));
        drop(upload_connections);
        drop(download_connections);
        info!("Added peer {:?}", peer);

        let port = self.get_new_port().await;
        self.ports.write().await.insert(peer, port);
        self.send_tree_announcement(peer, self.current_announcement().await)
            .await;

        self.spawn_peer(peer).await;
    }

    async fn spawn_peer(&self, peer: VerificationKey) {
        let router = self.clone();
        tokio::spawn(async move {
            loop {
                let running = router.running.read().await;
                if *running == false {
                    info!("Stopped router. Stopping peer {:?}", peer);
                    break;
                }
                drop(running);
                match router.poll_peer(peer).await {
                    Ok(_) => continue,
                    Err(_) => break,
                }
            }
        });
    }
    async fn poll_peer(&self, peer: VerificationKey) -> Result<(), ()> {
        let sockets = self.download_connections.read().await;
        if let Some(socket) = sockets.get(&peer) {
            if let Ok(result) = Self::poll_socket(socket).await {
                if let Some(decode_result) = result {
                    match decode_result {
                        Ok(frame) => {
                            trace!("Received {:?}", frame);
                            self.handle_frame(frame, peer).await;
                            return Ok(());
                        }
                        Err(e) => {
                            debug!("Could not decode {:?}", e);
                            return Ok(());
                        }
                    }
                } else {
                    debug!("Stream of {:?} ended. Stopping peer", peer);
                    return Err(());
                }
            } else {
                // Socket poll timeout exceeded
                return Ok(());
            }
        } else {
            debug!("No stream for {:?}. Stopping peer", peer);
            return Err(());
        }
    }
    async fn poll_socket(
        socket: &Mutex<DownloadConnection>,
    ) -> Result<Option<Result<Frame, std::io::Error>>, Elapsed> {
        let mut socket = socket.lock().await;
        let result = time::timeout(
            Duration::from_millis(10),
            async move { socket.next().await },
        )
        .await;
        result
    }

    async fn get_new_port(&self) -> Port {
        for i in 1.. {
            let mut used = false;
            for port in self.ports.read().await.values() {
                if port == &i {
                    used = true;
                }
            }
            match used {
                true => {
                    continue;
                }
                false => {
                    return i;
                }
            }
        }
        unreachable!("Reached port limit of {}", Port::MAX);
    }
    async fn disconnect_port(&self, port: Port) {
        let peer = self.get_peer_on_port(port).await.unwrap();
        let mut bootstrap = false;
        // Scan the local DHT table for any routes that transited this now-dead
        // peering. If we find any then we need to send teardowns in the opposite
        // direction, so that nodes further along the path will learn that the
        // path was broken.
        for (key, value) in self.paths.read().await.clone() {
            if value.destination == port || value.source == port {
                self.send_teardown_for_existing_path(port, key.public_key, key.path_id)
                    .await;
            }
        }

        // If the ascending path was also lost because it went via the now-dead
        // peering then clear that path (although we can't send a teardown) and
        // then bootstrap again.
        if let Some(asc) = &self.ascending_path.read().await.clone() {
            let ascending = self.paths.read().await.get(&asc).unwrap().clone();
            if ascending.destination == port {
                self.teardown_path(0, asc.public_key, asc.path_id).await;
                bootstrap = true;
            }
        }

        // If the descending path was lost because it went via the now-dead
        // peering then clear that path (although we can't send a teardown) and
        // wait for another incoming setup.
        if let Some(desc) = &self.descending_path.read().await.clone() {
            let descending = self.paths.read().await.get(&desc).unwrap().clone();
            if descending.destination == port {
                self.teardown_path(0, desc.public_key, desc.path_id).await;
            }
        }

        // If the peer that died was our chosen tree parent, then we will need to
        // select a new parent. If we successfully choose a new parent (as in, we
        // don't end up promoting ourselves to a root) then we will also need to
        // send a new bootstrap into the network.
        if self.parent().await == peer {
            bootstrap = bootstrap || self.parent_selection().await;
        }

        if bootstrap {
            self.bootstrap_now().await;
        }
    }

    async fn tree_announcement(&self, of: VerificationKey) -> Option<TreeAnnouncement> {
        self.announcements.read().await.get(&of).cloned()
    }
    async fn set_tree_announcement(&self, of: VerificationKey, announcement: TreeAnnouncement) {
        self.announcements
            .write()
            .await
            .insert(of.clone(), announcement);
    }
    async fn port(&self, of: VerificationKey) -> Option<Port> {
        if *self.public_key == of {
            return Some(0);
        }
        let ports = self.ports.read().await;
        ports.get(&of).cloned()
    }
    async fn peers(&self) -> Vec<VerificationKey> {
        let mut peers = Vec::new();
        for (peer, port) in &*self.ports.read().await {
            peers.push(peer.clone());
        }
        peers
    }
    async fn parent(&self) -> VerificationKey {
        *self.parent.read().await
    }
    async fn set_parent(&self, peer: VerificationKey) {
        trace!("Setting parent to {:?}", peer);
        *self.parent.write().await = peer;
    }
    fn public_key(&self) -> VerificationKey {
        *self.public_key
    }
    async fn get_peer_on_port(&self, port: Port) -> Option<VerificationKey> {
        if port == 0 {
            return Some(self.public_key());
        }
        for (peer, peer_port) in &*self.ports.read().await {
            if &port == peer_port {
                return Some(peer.clone());
            }
        }
        None
    }
    async fn current_sequence(&self) -> SequenceNumber {
        *self.sequence.read().await
    }
    async fn next_sequence(&self) -> SequenceNumber {
        {
            let mut sequence = self.sequence.write().await;
            *sequence += 1;
        }
        *self.sequence.read().await
    }
    async fn current_ordering(&self) -> SequenceNumber {
        *self.ordering.read().await
    }
    async fn next_ordering(&self) -> SequenceNumber {
        {
            let mut ordering = self.ordering.write().await;
            *ordering += 1;
        }
        *self.ordering.read().await
    }
    async fn reparent_timer_expired(&self) -> bool {
        let timer = self.reparent_timer.read().await;
        if let Some(timer) = &*timer {
            timer.is_expired()
        } else {
            true
        }
    }
    async fn set_reparent_timer(&self) {
        trace!("Reparent in {:?}", REPARENT_WAIT_TIME);
        *self.reparent_timer.write().await = Some(WaitTimer::new(REPARENT_WAIT_TIME));
    }
    async fn announcement_timer_expired(&self) -> bool {
        self.announcement_timer.read().await.is_expired()
    }
    async fn reset_announcement_timer(&self) {
        *self.announcement_timer.write().await = WaitTimer::new(ANNOUNCEMENT_INTERVAL);
    }
    async fn send_to_local(&self, frame: Frame) {
        self.download.send(frame).await.unwrap();
    }
    async fn send(&self, frame: Frame, to: VerificationKey) {
        let upload_connections = self.upload_connections.read().await;
        let socket = upload_connections.get(&to);
        if let Some(socket) = socket {
            trace!("Sending {:?}", frame);
            let mut socket = socket.lock().await;
            socket.send(frame).await;
        } else {
            debug!("No Socket for {:?}", to);
        }
    }

    async fn handle_frame(&self, frame: Frame, from: VerificationKey) {
        match frame {
            Frame::TreeRouted(packet) => {
                if let Some(peer) = self.next_tree_hop(&packet, from).await {
                    let peer = peer;
                    if peer == self.public_key() {
                        self.send_to_local(Frame::TreeRouted(packet)).await;
                        return;
                    }
                    self.send(Frame::TreeRouted(packet), peer).await;
                    return;
                }
                return;
            }
            Frame::SnekRouted(packet) => {
                if let Some(peer) = self.next_snek_hop(&packet, false, true).await {
                    let peer = peer;
                    if peer == *self.public_key {
                        self.send_to_local(Frame::SnekRouted(packet)).await;
                        return;
                    }
                    self.send(Frame::SnekRouted(packet), peer).await;
                    return;
                }
                return;
            }
            Frame::TreeAnnouncement(announcement) => {
                self.handle_tree_announcement(announcement, from).await;
            }

            Frame::SnekBootstrap(bootstrap) => {
                let next_hop = self.next_snek_hop(&bootstrap, true, false).await.unwrap();
                if next_hop == *self.public_key {
                    self.handle_snek_bootstrap(bootstrap).await;
                } else {
                    trace!("Forwarding SnekBootstrap.");
                    self.send(Frame::SnekBootstrap(bootstrap), next_hop).await;
                }
            }
            Frame::SnekBootstrapACK(ack) => {
                self.handle_snek_bootstrap_ack(ack).await;
            }
            Frame::SnekSetup(setup) => {
                let from_port = self.port(from).await.unwrap().clone();
                let next_hop = self.next_tree_hop(&setup, from).await.unwrap();
                let next_hop_port = self.port(next_hop).await.unwrap().clone();
                self.handle_setup(from_port, setup, next_hop_port).await;
            }
            Frame::SnekSetupACK(ack) => {
                let port = self.port(from).await.unwrap().clone();
                self.handle_setup_ack(port, ack).await;
            }
            Frame::SnekTeardown(teardown) => {
                let port = self.port(from).await.unwrap().clone();
                self.handle_teardown(port, teardown).await;
            }
        }
    }
    async fn next_tree_hop(
        &self,
        frame: &impl TreeRouted,
        from: VerificationKey,
    ) -> Option<VerificationKey> {
        if frame.destination_coordinates() == self.coordinates().await {
            return Some(self.public_key());
        }
        let our_distance = frame
            .destination_coordinates()
            .distance_to(&self.coordinates().await);
        if our_distance == 0 {
            return Some(self.public_key());
        }

        let mut best_peer = None;
        let mut best_distance = our_distance;
        let mut best_ordering = SequenceNumber::MAX;
        for peer in self.peers().await {
            if peer == from {
                continue; // don't route back where the packet came from
            }
            if let None = self.tree_announcement(peer).await {
                continue; // ignore peers that haven't sent us announcements
            }
            if let Some(announcement) = self.tree_announcement(peer).await {
                if !(self.current_root().await == announcement.root) {
                    continue; // ignore peers that are following a different root or seq
                }

                let peer_coordinates: Coordinates = announcement.peer_coords();
                let distance_to_peer =
                    peer_coordinates.distance_to(&frame.destination_coordinates());
                if Self::is_better_next_tree_hop_candidate(
                    distance_to_peer,
                    best_distance,
                    announcement.receive_order,
                    best_ordering,
                    best_peer.is_some(),
                ) {
                    best_peer = Some(peer);
                    best_distance = distance_to_peer;
                    best_ordering = announcement.receive_order;
                }
            }
        }
        best_peer
    }
    fn is_better_next_tree_hop_candidate(
        peer_distance: usize,
        best_distance: usize,
        peer_order: SequenceNumber,
        best_order: SequenceNumber,
        candidate_exists: bool,
    ) -> bool {
        let mut better_candidate = false;
        if peer_distance < best_distance {
            // The peer is closer to the destination.
            better_candidate = true;
        } else if peer_distance > best_distance {
            // The peer is further away from the destination.
        } else if candidate_exists && peer_order < best_order {
            // The peer has a lower latency path to the root as a
            // last-resort tiebreak.
            better_candidate = true;
        }
        better_candidate
    }
    async fn handle_tree_announcement(&self, mut frame: TreeAnnouncement, from: VerificationKey) {
        frame.receive_time = SystemTime::now();
        frame.receive_order = self.next_ordering().await;

        if let Some(announcement) = self.tree_announcement(from).await {
            if frame.has_same_root_key(&announcement) {
                if frame.replayed_old_sequence(&announcement) {
                    debug!("Announcement replayed old sequence. Dropping");
                    return;
                }
            }
        }
        trace!("Storing announcement {}", frame);
        self.set_tree_announcement(from, frame.clone()).await;
        if !self.reparent_timer_expired().await {
            debug!("Waiting to reparent");
            return;
        }
        if from == self.parent().await {
            trace!("Announcement came from parent");
            if frame.is_loop_of_child(&self.public_key()) {
                // SelectNewParentWithWait
                debug!("Announcement contains loop");
                self.become_root().await;
                self.reparent(true).await;
                return;
            }
            if frame.root.public_key < self.current_announcement().await.root.public_key {
                // SelectNewParentWithWait
                debug!("Announcement has weaker root");
                self.become_root().await;
                self.reparent(true).await;
                return;
            }
            if frame.root.public_key > self.current_announcement().await.root.public_key {
                // AcceptUpdate
                debug!("Announcement has stronger root. Forwarding to peers");
                self.send_tree_announcements_to_all(self.current_announcement().await)
                    .await;
                return;
            }
            if frame.root.public_key == self.current_announcement().await.root.public_key {
                if frame.root.sequence_number
                    > self.current_announcement().await.root.sequence_number
                {
                    // AcceptUpdate
                    trace!("Announcement has higher sequence. Forwarding to peers");
                    self.send_tree_announcements_to_all(self.current_announcement().await)
                        .await;
                    return;
                }
                // SelectNewParentWithWait
                trace!("Announcement replayed current sequence");
                self.become_root().await;
                self.reparent(true).await;
                return;
            }
        } else {
            trace!("Announcement didn't come from parent");
            if frame.is_loop_of_child(&self.public_key()) {
                // DropFrame
                trace!("Announcement contains loop. Dropping");
                return;
            }
            if frame.root.public_key > self.current_announcement().await.root.public_key {
                // AcceptNewParent
                trace!("Announcement has stronger root. Forwarding to peers");
                self.set_parent(from.clone()).await;
                let announcement = self.current_announcement().await;
                self.send_tree_announcements_to_all(announcement).await;
                return;
            }
            if frame.root.public_key < self.current_announcement().await.root.public_key {
                // InformPeerOfStrongerRoot
                trace!("Announcement has weaker root. Sending my announcement");
                self.send_tree_announcement(from, self.current_announcement().await)
                    .await;
                return;
            }
            if frame.root.public_key == self.current_announcement().await.root.public_key {
                // SelectNewParent
                trace!("Announcement has same root");
                self.reparent(false).await;
                return;
            }
        }
    }
    async fn current_announcement(&self) -> TreeAnnouncement {
        if let Some(announcement) = self.tree_announcement(self.parent().await).await {
            announcement.clone()
        } else {
            TreeAnnouncement {
                root: Root {
                    public_key: self.public_key(),
                    sequence_number: self.current_sequence().await,
                },
                signatures: vec![],
                receive_time: SystemTime::now(),
                receive_order: self.current_ordering().await,
            }
        }
    }
    async fn coordinates(&self) -> Coordinates {
        self.current_announcement().await.coords()
    }
    async fn send_tree_announcements_to_all(&self, announcement: TreeAnnouncement) {
        trace!("Sending tree announcements to all peers");
        for peer in self.peers().await {
            self.send_tree_announcement(peer, announcement.clone())
                .await;
        }
    }
    async fn send_tree_announcement(&self, to: VerificationKey, announcement: TreeAnnouncement) {
        let port = self.port(to).await.unwrap();
        let signed_announcement = announcement.append_signature(self.public_key(), port);
        trace!("Sending tree announcement to port {}", port);
        self.send(Frame::TreeAnnouncement(signed_announcement), to)
            .await;
    }
    async fn new_tree_announcement(&self) -> TreeAnnouncement {
        TreeAnnouncement {
            root: Root {
                public_key: self.public_key(),
                sequence_number: self.next_sequence().await,
            },
            signatures: vec![],
            receive_time: SystemTime::now(),
            receive_order: 0,
        }
    }
    async fn parent_selection(&self) -> bool {
        trace!("Running parent selection...");
        if self.public_key() > self.current_root().await.public_key {
            debug!("My key is stronger than current root");
            self.become_root().await;
        }
        let mut best_root = self.current_root().await;
        let mut best_peer = None;
        let mut best_order = SequenceNumber::MAX;
        for peer in self.peers().await {
            if let Some(announcement) = self.tree_announcement(peer).await {
                if announcement.receive_time.elapsed().unwrap() > ANNOUNCEMENT_TIMEOUT {
                    continue;
                }
                if announcement.is_loop_of_child(&self.public_key()) {
                    continue;
                }
                if announcement.root > best_root {
                    best_root = announcement.root.clone();
                    best_peer = Some(peer);
                    best_order = announcement.receive_order;
                }
                if announcement.root < best_root {
                    continue;
                }
                if announcement.receive_order < best_order {
                    best_root = announcement.root.clone();
                    best_peer = Some(peer);
                    best_order = announcement.receive_order;
                }
            }
        }
        match best_peer {
            Some(best_peer) => {
                if best_peer == self.parent().await {
                    debug!("Current parent is the best available parent");
                    return false;
                }
                let best_peer = best_peer.clone();
                self.set_parent(best_peer).await;
                self.send_tree_announcements_to_all(self.current_announcement().await)
                    .await;
                return true;
            }
            None => {
                debug!("I am root");
                self.become_root().await;
                return false;
            }
        }
    }
    async fn become_root(&self) {
        trace!("Becoming root");
        self.set_parent(self.public_key().clone()).await;
    }
    async fn reparent(&self, wait: bool) {
        let router = self.clone();
        tokio::spawn(async move {
            if wait {
                trace!("Waiting to reparent");
                sleep(REPARENT_WAIT_TIME).await;
            }
            trace!("Re-parenting");
            if router.parent_selection().await {
                router.bootstrap_now().await;
            }
        });
    }
    async fn current_root(&self) -> Root {
        self.current_announcement().await.root
    }
    async fn maintain_tree(&self) {
        if self.i_am_root().await {
            let announcement = self.new_tree_announcement().await;
            self.send_tree_announcements_to_all(announcement).await;
        }
        self.reparent(true).await;
    }
    async fn i_am_root(&self) -> bool {
        self.public_key() == self.parent().await
    }

    /// `maintain_snake` is responsible for working out if we need to send bootstraps
    /// or to clean up any old paths.
    async fn maintain_snek(&self) {
        // Work out if we are able to bootstrap. If we are the root node then
        // we don't send bootstraps, since there's nowhere for them to go —
        // bootstraps are sent up to the next ascending node, but as the root,
        // we already have the highest key on the network.
        let root_announcement = self.current_announcement().await;
        let can_bootstrap = self.parent().await != self.public_key()
            && root_announcement.root.public_key != self.public_key();
        let mut will_bootstrap = false;

        // The ascending node is the node with the next highest key.
        if let Some(asc) = &self.ascending_path.read().await.clone() {
            let ascending = self.paths.read().await.get(&asc).unwrap().clone();
            if !ascending.valid() {
                // The ascending path entry has expired, so tear it down and then
                // see if we can bootstrap again.
                trace!("Ascending path expired. Tearing down and bootstrapping.");
                self.send_teardown_for_existing_path(0, asc.public_key, asc.path_id)
                    .await;
                will_bootstrap = can_bootstrap;
            }
            if ascending.root != root_announcement.root {
                // The ascending node was set up with a different root key or sequence
                // number. In this case, we will send another bootstrap to the remote
                // side in order to hopefully replace the path with a new one.
                trace!("Ascending path has different root. Bootstrapping.");
                will_bootstrap = can_bootstrap;
            }
        } else {
            // We don't have an ascending node at all, so if we can, we'll try
            // bootstrapping to locate it.
            trace!("No ascending path. Bootstrapping.");
            will_bootstrap = can_bootstrap;
        }

        // The descending node is the node with the next lowest key.
        if let Some(desc) = &self.descending_path.read().await.clone() {
            let descending_path = self.paths.read().await.get(&desc).unwrap().clone();
            if !descending_path.valid() {
                // The descending path has expired, so tear it down and then that should
                // prompt the remote side into sending a new bootstrap to set up a new
                // path, if they are still alive.
                trace!("Tearing down expired descending path. Wait for bootstrap.");
                self.send_teardown_for_existing_path(0, desc.public_key, desc.path_id)
                    .await;
            }
        }

        // Clean up any paths that were installed more than 5 seconds ago but haven't
        // been activated by a setup ACK.
        for (index, path) in self.paths.read().await.clone() {
            if !path.active && path.last_seen.elapsed().unwrap() > Duration::from_secs(5) {
                trace!("Tearing down old inactive path.");
                self.send_teardown_for_existing_path(0, index.public_key, index.path_id)
                    .await;
            }
        }

        // If one of the previous conditions means that we need to bootstrap, then
        // send the actual bootstrap message into the network.
        if will_bootstrap {
            self.bootstrap_now().await;
        }
    }

    /// `bootstrap_now` is responsible for sending a bootstrap massage to the network
    async fn bootstrap_now(&self) {
        trace!("Bootstrapping ...");
        // If we are the root node then there's no point in trying to bootstrap. We
        // already have the highest public key on the network so a bootstrap won't be
        // able to go anywhere in ascending order.
        if self.parent().await == self.public_key() {
            trace!("Not bootstrapping because I am root");
            return;
        }

        // If we already have a relationship with an ascending node and that has the
        // same root key and sequence number (i.e. nothing has changed in the tree since
        // the path was set up) then we don't need to send another bootstrap message just
        // yet. We'll either wait for the path to be torn down, expire or for the tree to
        // change.
        let announcement = self.current_announcement().await;
        if let Some(asc) = &*self.ascending_path.read().await {
            let paths = self.paths.read().await;
            let ascending = paths.get(&asc).unwrap();
            let asc_peer = self.get_peer_on_port(ascending.source).await.unwrap();
            if ascending.root == announcement.root {
                trace!("Not bootstrapping because a valid ascending path is set");
                return;
            }
        }

        // Construct the bootstrap packet. We will include our root key and sequence
        // number in the update so that the remote side can determine if we are both using
        // the same root node when processing the update.
        let frame = SnekBootstrap {
            root: self.current_root().await,
            destination_key: self.public_key(),
            source: self.coordinates().await,
            path_id: thread_rng().gen(),
        };

        if let Some(peer) = self.next_snek_hop(&frame, true, false).await {
            trace!("Bootstrapping path {} ", frame.path_id);
            self.send(Frame::SnekBootstrap(frame), peer).await;
        } else {
            trace!("Not bootstrapping because no next hop was found");
        }
    }

    async fn next_snek_hop(
        &self,
        frame: &impl SnekRouted,
        bootstrap: bool,
        traffic: bool,
    ) -> Option<VerificationKey> {
        let destination_key = frame.destination_key();
        // If the message isn't a bootstrap message and the destination is for our
        // own public key, handle the frame locally — it's basically loopback.
        if !bootstrap && self.public_key() == destination_key {
            return Some(self.public_key());
        }

        // We start off with our own key as the best key. Any suitable next-hop
        // candidate has to improve on our own key in order to forward the frame.
        let mut best_peer = None;
        if !traffic {
            best_peer = Some(self.public_key());
        }
        let mut best_key = self.public_key();

        // Check if we can use the path to the root via our parent as a starting
        // point. We can't do this if we are the root node as there would be no
        // parent or ascending paths.
        if self.parent().await != self.public_key() {
            if bootstrap && best_key == destination_key {
                // Bootstraps always start working towards their root so that they
                // go somewhere rather than getting stuck.
                best_key = self.current_announcement().await.root.public_key;
                best_peer = Some(self.parent().await)
            }
            if Self::dht_ordered(
                &best_key,
                &destination_key,
                &self.current_announcement().await.root.public_key,
            ) {
                // The destination key is higher than our own key, so start using
                // the path to the root as the first candidate.
                best_key = self.current_announcement().await.root.public_key;
                best_peer = Some(self.parent().await)
            }

            // Check our direct ancestors in the tree, that is, all nodes between
            // ourselves and the root node via the parent port.
            for ancestor in self
                .current_announcement()
                .await
                .signatures
                .iter()
                .map(|x| x.signing_public_key)
            {
                if !bootstrap && ancestor == destination_key && best_key != destination_key {
                    best_key = ancestor;
                    best_peer = Some(self.parent().await);
                }
                if Self::dht_ordered(&destination_key, &ancestor, &best_key) {
                    best_key = ancestor;
                    best_peer = Some(self.parent().await);
                }
            }
        }

        // Check all of the ancestors of our direct peers too, that is, all nodes
        // between our direct peer and the root node.
        for (peer, announcement) in &*self.announcements.read().await {
            for hop in &announcement.signatures {
                if !bootstrap
                    && hop.signing_public_key == destination_key
                    && best_key != destination_key
                {
                    best_key = hop.signing_public_key;
                    best_peer = Some(peer.clone());
                }
                if Self::dht_ordered(&destination_key, &hop.signing_public_key, &best_key) {
                    best_key = hop.signing_public_key;
                    best_peer = Some(peer.clone());
                }
            }
        }

        // Check whether our current best candidate is actually a direct peer.
        // This might happen if we spotted the node in our direct ancestors for
        // example, only in this case it would make more sense to route directly
        // to the peer via our peering with them as opposed to routing via our
        // parent port.
        for peer in self.peers().await {
            if best_key == peer {
                best_key = peer;
                best_peer = Some(peer);
            }
        }

        // Check our DHT entries. In particular, we are only looking at the source
        // side of the DHT paths. Since setups travel from the lower key to the
        // higher one, this is effectively looking for paths that descend through
        // keyspace toward lower keys rather than ascend toward higher ones.
        for (key, entry) in &*self.paths.read().await {
            if !entry.valid() || entry.source == 0 {
                continue;
            }
            if !bootstrap && !entry.active {
                continue;
            }
            if !bootstrap && key.public_key == destination_key && best_key != destination_key {
                best_key = key.public_key;
                best_peer = Some(self.get_peer_on_port(entry.source).await.unwrap());
            }
            if Self::dht_ordered(&destination_key, &key.public_key, &best_key) {
                best_key = key.public_key;
                best_peer = Some(self.get_peer_on_port(entry.source).await.unwrap());
            }
        }
        best_peer
    }

    /// `handle_bootstrap` is called in response to receiving a bootstrap packet.
    /// This function will send a bootstrap ACK back to the sender.
    async fn handle_snek_bootstrap(&self, frame: SnekBootstrap) {
        // Check that the root key and sequence number in the update match our
        // current root, otherwise we won't be able to route back to them using
        // tree routing anyway. If they don't match, silently drop the bootstrap.
        if self.current_root().await == frame.root {
            // In response to a bootstrap, we'll send back a bootstrap ACK packet to
            // the sender. We'll include our own root details in the ACK.
            let frame = SnekBootstrapAck {
                // Bootstrap ACKs are routed using tree routing, so we need to take the
                // coordinates from the source field of the received packet and set the
                // destination of the ACK packet to that.
                destination_coordinates: frame.source.clone(),
                destination_key: frame.destination_key,
                source_coordinates: self.coordinates().await,
                source_key: self.public_key(),
                root: self.current_root().await,
                path_id: frame.path_id,
            };
            if let Some(peer) = self.next_tree_hop(&frame, self.public_key()).await {
                trace!("Responding to SnekBootstrap with Ack.");
                self.send(Frame::SnekBootstrapACK(frame), peer).await;
            } else {
                debug!("No next tree hop for BootstrapAck");
            }
        } else {
            trace!("Bootstrap doesn't have same root. Dropping");
        }
    }

    /// `handle_snek_bootstrap_ack` is called in response to receiving a bootstrap ACK
    /// packet. This function will work out whether the remote node is a suitable
    /// candidate to set up an outbound path to, and if so, will send path setup
    /// packets to the network.
    async fn handle_snek_bootstrap_ack(&self, ack: SnekBootstrapAck) {
        let ascending_path = self.ascending_path.read().await;
        let mut paths = self.paths.write().await;
        let mut update = false;
        if ack.source_key == self.public_key() {
            // We received a bootstrap ACK from ourselves. This shouldn't happen,
            // so either another node has forwarded it to us incorrectly, or
            // a routing loop has occurred somewhere. Don't act on the bootstrap
            // in that case.
            trace!("Received own bootstrap ack. Dropping");
        } else if ack.root != self.current_root().await {
            // The root key in the bootstrap ACK doesn't match our own key, or the
            // sequence doesn't match, so it is quite possible that routing setup packets
            // using tree routing would fail.
            trace!("Bootstrap-ack doesn't have same root. Dropping");
        } else if let Some(asc) = &*ascending_path {
            if let Some(ascending) = paths.get(&asc) {
                if ascending.valid() {
                    // We already have an ascending entry and it hasn't expired yet.
                    if ascending.origin == ack.source_key && ack.path_id != asc.path_id {
                        // We've received another bootstrap ACK from our direct ascending node.
                        // Just refresh the record and then send a new path setup message to
                        // that node.
                        trace!("Received updated bootstrap-ack from current ascending node. Sending new path setup.");
                        update = true
                    } else if Self::dht_ordered(
                        &self.public_key(),
                        &ack.source_key,
                        &ascending.origin,
                    ) {
                        // We know about an ascending node already but it turns out that this
                        // new node that we've received a bootstrap from is actually closer to
                        // us than the previous node. We'll update our record to use the new
                        // node instead and then send a new path setup message to it.
                        trace!("Received bootstrap-ack from closer node. Updating ascending path and sending new path setup.");
                        update = true;
                    }
                } else {
                    // Ascending Path expired.
                    if self.public_key() < ack.source_key {
                        // We don't know about an ascending node and at the moment we don't know
                        // any better candidates, so we'll accept a bootstrap ACK from a node with a
                        // key higher than ours (so that it matches descending order).
                        trace!("Current ascending path expired. Accepting bootstrap-ack from valid peer.");
                        update = true;
                    }
                }
            }
        } else if None == *ascending_path {
            // We don't have an ascending entry
            if self.public_key() < ack.source_key {
                // We don't know about an ascending node and at the moment we don't know
                // any better candidates, so we'll accept a bootstrap ACK from a node with a
                // key higher than ours (so that it matches descending order).
                trace!("Accepting bootstrap-ack from valid peer.");
                update = true;
            }
        } else {
            // The bootstrap ACK conditions weren't met. This might just be because
            // there's a node out there that hasn't converged to a closer node
            // yet, so we'll just ignore the acknowledgement.
            trace!("Dropping non-valid bootstrap-ack.");
        }
        if !update {
            return;
        }
        // Setup messages routed using tree routing. The destination key is set in the
        // header so that a node can determine if the setup message arrived at the
        // intended destination instead of forwarding it. The source key is set to our
        // public key, since this is the lower of the two keys that intermediate nodes
        // will populate into their routing tables.
        let setup = SnekSetup {
            root: self.current_root().await,
            destination: ack.source_coordinates,
            destination_key: ack.source_key.clone(),
            source_key: self.public_key(),
            path_id: ack.path_id,
        };
        let next_hop = self.next_tree_hop(&setup, self.public_key()).await;

        // Importantly, we will only create a DHT entry if it appears as though our next
        // hop has actually accepted the packet. Otherwise we'll create a path entry and
        // the setup message won't go anywhere.
        match next_hop {
            None => {
                // No peer was identified, which shouldn't happen.
                debug!("No next tree hop for SnekSetup");
                return;
            }
            Some(next_peer) => {
                if self.public_key() == next_peer {
                    // The peer is local, which shouldn't happen.
                    debug!("Next hop for SnekSetup is self. Dropping.");
                    return;
                }
                self.send(Frame::SnekSetup(setup), next_peer).await;
                let index = SnekPathIndex {
                    public_key: self.public_key(),
                    path_id: ack.path_id.clone(),
                };
                let entry = SnekPath {
                    origin: ack.source_key,
                    target: ack.source_key,
                    source: 0,
                    destination: self.port(next_peer).await.unwrap().clone(),
                    last_seen: SystemTime::now(),
                    root: ack.root.clone(),
                    active: false,
                };
                // The remote side is responsible for clearing up the replaced path, but
                // we do want to make sure we don't have any old paths to other nodes
                // that *aren't* the new ascending node lying around. This helps to avoid
                // routing loops.
                for (dht_key, entry) in &paths.clone() {
                    if entry.source == 0
                        && dht_key.public_key /*TODO dht_key.public_key OR entry.public_key which doesn't exist*/
                        != ack.source_key
                    {
                        self.send_teardown_for_existing_path(
                            0,
                            dht_key.public_key,
                            dht_key.path_id,
                        )
                        .await;
                    }
                }
                // Install the new route into the DHT.
                trace!("Adding route {:?} to DHT", index);
                paths.insert(index, entry.clone());
                *self.candidate.write().await = Some(entry);
            }
        }
    }

    /// `handle_setup` is called in response to receiving setup packets. Note that
    /// these packets are handled even as we forward them, as setup packets should be
    /// processed by each node on the path.
    async fn handle_setup(&self, from: Port, rx: SnekSetup, next_hop: Port) {
        let mut descending_path = self.descending_path.write().await;
        let mut paths = self.paths.write().await;
        if self.current_root().await != rx.root {
            trace!("SnekSetup has different root. Responding with Teardown");
            self.send_teardown_for_rejected_path(rx.source_key, rx.path_id, from)
                .await;
        }
        let index = SnekPathIndex {
            public_key: rx.source_key,
            path_id: rx.path_id,
        };
        // If we already have a path for this public key and path ID combo, which
        // *shouldn't* happen, then we need to tear down both the existing path and
        // then send back a teardown to the sender notifying them that there was a
        // problem. This will probably trigger a new setup, but that's OK, it should
        // have a new path ID.
        if paths.contains_key(&index) {
            trace!("Trigger new SnekSetup because of already existing path.");
            self.send_teardown_for_existing_path(0, rx.source_key, rx.path_id)
                .await;
            self.send_teardown_for_rejected_path(rx.source_key, rx.path_id, from)
                .await;
            return;
        }
        // If we're at the destination of the setup then update our predecessor
        // with information from the bootstrap.
        if rx.destination_key == self.public_key() {
            let mut update = false;
            if self.current_root().await != rx.root {
                // The root key in the bootstrap ACK doesn't match our own key, or the
                // sequence doesn't match, so it is quite possible that routing setup packets
                // using tree routing would fail.
                trace!("SnekSetup has different root. Dropping.");
            } else if !(rx.source_key < self.public_key()) {
                // The bootstrapping key should be less than ours but it isn't.
                trace!("Key of bootstrapping node is not less then self. Dropping.");
            } else if let Some(desc) = &*descending_path {
                let descending = paths.get(desc).unwrap();
                if descending.valid() {
                    // We already have a descending entry and it hasn't expired.
                    if desc.public_key == rx.source_key && rx.path_id != desc.path_id {
                        // We've received another bootstrap from our direct descending node.
                        // Send back an acknowledgement as this is OK.
                        trace!("Received another SnekSetup from current descending node. Responding with SnekSetupAck.");
                        update = true;
                    } else if Self::dht_ordered(
                        &desc.public_key,
                        &rx.source_key,
                        &self.public_key(),
                    ) {
                        // The bootstrapping node is closer to us than our previous descending
                        // node was.
                        trace!("Received SnekSetup from closer node. Accepting.");
                        update = true;
                    }
                } else {
                    // Our descending entry has expired
                    if rx.source_key < self.public_key() {
                        // The bootstrapping key is less than ours so we'll acknowledge it.
                        trace!("Descending entry expired. Accepting SnekSetup.");
                        update = true;
                    } else {
                        trace!("Descending entry expired but received SnekSetup isn't dht-ordered. Dropping.");
                    }
                }
            } else if let None = *descending_path {
                // We don't have a descending entry
                if rx.source_key < self.public_key() {
                    // The bootstrapping key is less than ours so we'll acknowledge it.
                    trace!("No descending entry. Accepting SnekSetup.");
                    update = true;
                } else {
                    trace!(
                        "No descending entry but received SnekSetup isn't dht-ordered. Dropping."
                    )
                }
            } else {
                // The bootstrap conditions weren't met. This might just be because
                // there's a node out there that hasn't converged to a closer node
                // yet, so we'll just ignore the bootstrap.
                trace!("Dropping non-valid SnekSetup.");
            }
            if !update {
                self.send_teardown_for_rejected_path(rx.source_key, rx.path_id, from)
                    .await;
                return;
            }
            if let Some(previous_path) = &descending_path.clone() {
                self.send_teardown_for_existing_path(
                    0,
                    previous_path.public_key,
                    previous_path.path_id,
                )
                .await;
            }
            let entry = SnekPath {
                origin: rx.source_key,
                target: rx.destination_key,
                source: from.clone(),
                destination: 0,
                last_seen: SystemTime::now(),
                root: rx.root.clone(),
                active: false,
            };
            paths.insert(index.clone(), entry.clone());
            *descending_path = Some(index.clone());
            // Send back a setup ACK to the remote side
            let setup_ack = SnekSetupAck {
                root: rx.root.clone(),
                destination_key: rx.source_key,
                path_id: index.path_id,
            };
            self.send(
                Frame::SnekSetupACK(setup_ack),
                self.get_peer_on_port(entry.source).await.unwrap(),
            )
            .await;
            return;
        }

        // Try to forward the setup onto the next node first. If we
        // can't do that then there's no point in keeping the path.
        let next_peer = self.get_peer_on_port(next_hop).await.unwrap();
        if next_peer == self.public_key() {
            debug!("Can't forward SnekSetup. Tearing down path.");
            self.send_teardown_for_rejected_path(rx.source_key, rx.path_id, from)
                .await;
            return;
        } else {
            trace!("Forwarding SnekSetup.");
            self.send(Frame::SnekSetup(rx.clone()), next_peer).await;
        }
        // Add a new routing table entry as we are intermediate to
        // the path.
        let entry = SnekPath {
            origin: rx.source_key,
            target: rx.destination_key,
            source: from,          // node with lower of the two keys
            destination: next_hop, // node with higher of the two keys
            last_seen: SystemTime::now(),
            root: rx.root,
            active: false,
        };
        paths.insert(index, entry);
    }

    /// `handle_setup_ack` is called in response to a setup ACK
    /// packet from the network
    async fn handle_setup_ack(&self, from: Port, rx: SnekSetupAck) {
        let mut paths = self.paths.write().await;
        // Look up to see if we have a matching route. The route must be not active
        // (i.e. we haven't received a setup ACK for it yet) and must have arrived
        // from the port that the entry was populated with.
        for (key, entry) in &mut *paths {
            if entry.active || key.public_key != rx.destination_key || key.path_id != rx.path_id {
                continue;
            }
            if from == entry.destination || from == 0 {
                if entry.source != 0 {
                    trace!("Forwarding SetupAck.");
                    let entry_source = self.get_peer_on_port(entry.source).await.unwrap();
                    self.send(Frame::SnekSetupACK(rx.clone()), entry_source)
                        .await;
                }
                trace!("Activating Path {:?}", key);
                entry.active = true;
                let mut candidate = self.candidate.write().await;
                if let Some(candidate_path) = &*candidate {
                    if entry == candidate_path {
                        *self.ascending_path.write().await = Some(key.clone());
                        *candidate = None;
                    }
                }
            }
        }
    }

    /// `handle_teardown` is called in response to receiving a teardown
    /// packet from the network
    async fn handle_teardown(&self, from: Port, rx: SnekTeardown) -> Vec<Port> {
        self.teardown_path(from, rx.destination_key, rx.path_id)
            .await
    }

    /// `teardown_path` processes a teardown message by tearing down any
    /// related routes, returning a slice of next-hop candidates that the
    /// teardown must be forwarded to.
    async fn teardown_path(
        &self,
        from: Port,
        path_key: VerificationKey,
        path_id: SnekPathId,
    ) -> Vec<Port> {
        let mut ascending_path = self.ascending_path.write().await;
        let mut descending_path = self.descending_path.write().await;
        let mut paths = self.paths.write().await;
        if let Some(asc) = &*ascending_path {
            if asc.public_key == path_key && asc.path_id == path_id {
                if from == 0 {
                    // originated locally
                }
                let ascending = paths.get(asc).unwrap().clone();
                if from == ascending.destination {
                    // from network
                    paths.remove(asc);
                    *ascending_path = None;
                    return vec![ascending.destination];
                }
            }
        }
        if let Some(desc) = &*descending_path {
            if desc.public_key == path_key && desc.path_id == path_id {
                if from == 0 {
                    // originated locally
                }
                let descending = paths.get(desc).unwrap().clone();
                if from == descending.destination {
                    // from network
                    paths.remove(desc);
                    *descending_path = None;
                    return vec![descending.destination];
                }
            }
        }
        for (key, value) in paths.to_owned() {
            if key.public_key == path_key && key.path_id == path_id {
                if from == 0 {
                    // happens when we're tearing down an existing duplicate path
                    paths.remove(&key);
                    return vec![value.destination, value.source];
                }
                if from == value.source {
                    // from network, return the opposite direction
                    paths.remove(&key);
                    return vec![value.destination];
                }
                if from == value.destination {
                    // from network, return the opposite direction
                    paths.remove(&key);
                    return vec![value.source];
                }
            }
        }
        return vec![];
    }

    async fn send_teardown_for_existing_path(
        &self,
        from: Port,
        path_key: VerificationKey,
        path_id: SnekPathId,
    ) {
        let frame = self.get_teardown(path_key, path_id).await;
        for next_hop in self.teardown_path(from, path_key, path_id).await {
            let peer = self.get_peer_on_port(next_hop).await.unwrap();
            self.send(Frame::SnekTeardown(frame.clone()), peer).await;
        }
    }
    async fn send_teardown_for_rejected_path(
        &self,
        path_key: VerificationKey,
        path_id: SnekPathId,
        via: Port,
    ) {
        let frame = self.get_teardown(path_key, path_id).await;
        let peer = self.get_peer_on_port(via).await.unwrap();
        self.send(Frame::SnekTeardown(frame), peer).await;
    }

    async fn get_teardown(&self, path_key: VerificationKey, path_id: SnekPathId) -> SnekTeardown {
        SnekTeardown {
            root: self.current_root().await,
            destination_key: path_key,
            path_id,
        }
    }
    /// `dht_ordered` returns true if the order of A, B and C is
    /// correct, where A < B < C without wrapping.
    fn dht_ordered(a: &VerificationKey, b: &VerificationKey, c: &VerificationKey) -> bool {
        a < b && b < c
    }
}
