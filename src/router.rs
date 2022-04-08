use crate::coordinates::Coordinates;
use crate::frames::{
    Frame, SnekBootstrap, SnekBootstrapAck, SnekSetup, SnekSetupAck, SnekTeardown,
};
use crate::snek::{SnekPath, SnekPathIndex, SnekRouted};
use crate::tree::TreeRouted;
use crate::wait_timer::WaitTimer;
use crate::{PineconeCodec, Root, TreeAnnouncement};
use futures::SinkExt;
use log::{debug, info, trace};
use rand::{thread_rng, Rng};
use std::collections::HashMap;
use std::io::Error;
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tokio::net::TcpStream;
use tokio::sync::mpsc::{Receiver, Sender};
use tokio::sync::{Mutex, RwLock};
use tokio::task::JoinHandle;
use tokio::time::error::Elapsed;
use tokio::{join, time};
use tokio::time::{interval, sleep};
use tokio_stream::StreamExt;
use tokio_util::codec::Framed;

pub type Port = u64;
pub type SequenceNumber = u64;
pub type SnekPathId = u64;
pub type VerificationKey = [u8; 32];

pub(crate) const SNEK_EXPIRY_PERIOD: Duration = Duration::from_secs(60 * 60);

pub(crate) const ANNOUNCEMENT_TIMEOUT: Duration = Duration::from_secs(45 * 60); //45 min
pub(crate) const ANNOUNCEMENT_INTERVAL: Duration = Duration::from_secs(30 * 60); // 30 min
pub(crate) const REPARENT_WAIT_TIME: Duration = Duration::from_secs(1); //   1 sec

#[derive(Debug)]
pub enum Event {
    AddPeer {
        key: VerificationKey,
        socket: Framed<TcpStream, PineconeCodec>,
    },
    HandleFrame(Frame),
    SendFrameToPeer {
        frame: Frame,
        to: VerificationKey,
    },
    RemovePeer(VerificationKey),
    StopRouter,
}
#[derive(Clone)]
struct SwitchState {
    pub(crate) public_key: VerificationKey,
    pub(crate) download: Arc<Sender<Frame>>,
    pub(crate) sockets:
        Arc<RwLock<HashMap<VerificationKey, Mutex<Framed<TcpStream, PineconeCodec>>>>>,
    ports: Arc<RwLock<HashMap<VerificationKey, Port>>>,
}
#[derive(Clone)]
struct TreeState {
    parent: Arc<RwLock<VerificationKey>>,
    announcements: Arc<RwLock<HashMap<VerificationKey, TreeAnnouncement>>>,
    sequence: Arc<RwLock<SequenceNumber>>,
    ordering: Arc<RwLock<SequenceNumber>>,
    announcement_timer: Arc<RwLock<WaitTimer>>,
    reparent_timer: Arc<RwLock<Option<WaitTimer>>>,
    reparent_handle: Arc<Mutex<Option<JoinHandle<()>>>>,
}
#[derive(Clone)]
struct SnekState {
    ascending_path: Arc<RwLock<Option<SnekPathIndex>>>,
    descending_path: Arc<RwLock<Option<SnekPathIndex>>>,
    paths: Arc<RwLock<HashMap<SnekPathIndex, SnekPath>>>,
    candidate: Arc<RwLock<Option<SnekPath>>>,
}
pub struct Router {
    peer_handles: Mutex<Vec<JoinHandle<()>>>,

    pub(crate) upload: Receiver<Event>,

    switch: SwitchState,
    tree: TreeState,
    snek: SnekState,
}
impl Router {
    pub fn new(key: VerificationKey, download: Sender<Frame>, upload: Receiver<Event>) -> Self {
        Self {
            peer_handles: Default::default(),
            upload,
            switch: SwitchState {
                public_key: key,
                download: Arc::new(download),
                sockets: Default::default(),
                ports: Default::default(),
            },
            tree: TreeState {
                parent: Arc::new(RwLock::new(key)),
                announcements: Default::default(),
                sequence: Arc::new(RwLock::new(0)),
                ordering: Arc::new(RwLock::new(0)),
                announcement_timer: Arc::new(RwLock::new(WaitTimer::new(ANNOUNCEMENT_INTERVAL))),
                reparent_timer: Arc::new(RwLock::new(None)),
                reparent_handle: Arc::new(Mutex::new(None)),
            },
            snek: SnekState {
                ascending_path: Arc::new(RwLock::new(None)),
                descending_path: Arc::new(RwLock::new(None)),
                paths: Arc::new(Default::default()),
                candidate: Arc::new(RwLock::new(None)),
            },
        }
    }
    pub fn spawn(mut self) -> JoinHandle<Self> {
        tokio::spawn(async move {
            loop {
                if let Some(event) = self.upload.recv().await {
                    match event {
                        Event::AddPeer { key, socket } => {
                            self.add_peer(key, socket).await;
                        }
                        Event::HandleFrame(frame) => {
                            Self::handle_frame(
                                frame,
                                self.switch.public_key,
                                &self.switch,
                                &self.tree,
                                &self.snek,
                            )
                            .await;
                        }
                        Event::RemovePeer(_) => {}
                        Event::SendFrameToPeer { frame, to: peer } => {
                            if let Some(socket) = self.switch.sockets.read().await.get(&peer) {
                                socket.lock().await.send(frame).await.unwrap();
                            } else {
                                debug!("No socket for {:?}", peer);
                            }
                        }
                        Event::StopRouter => {
                            break;
                        }
                    }
                } else {
                    debug!("Local event channel closed.");
                    break;
                }
            }
            trace!("Stopping router");
            self.stop_router().await;
            self
        })
    }
    async fn stop_router(&self) {
        let mut handles = self.peer_handles.lock().await;
        for handle in handles.pop() {
            handle.await.unwrap();
        }
    }
    async fn add_peer(&self, peer: VerificationKey, socket: Framed<TcpStream, PineconeCodec>) {
        let mut sockets = self.switch.sockets.write().await;
        if sockets.contains_key(&peer) {
            info!("Couldn't add {:?} because it already exists", peer);
            return;
        }
        sockets.insert(peer, Mutex::new(socket));
        drop(sockets);
        info!("Added peer {:?}", peer);

        let port = Self::get_new_port(&self.switch).await;
        self.switch.ports.write().await.insert(peer, port);
        Self::send_tree_announcement(
            peer,
            Self::current_announcement(&self.switch, &self.tree).await,
            &self.switch,
        )
        .await;

        self.spawn_peer(peer).await;
    }
    async fn spawn_peer(&self, peer: VerificationKey) {
        let switch = self.switch.clone();
        let tree = self.tree.clone();
        let snek = self.snek.clone();
        self.peer_handles
            .lock()
            .await
            .push(tokio::spawn(async move {
                loop {
                    match Self::poll_peer(peer, &switch, &tree, &snek).await {
                        Ok(_) => continue,
                        Err(_) => break,
                    }
                }
            }));
    }
    async fn poll_peer(
        peer: VerificationKey,
        switch: &SwitchState,
        tree: &TreeState,
        snek: &SnekState,
    ) -> Result<(), ()> {
        let sockets = switch.sockets.read().await;
        if let Some(socket) = sockets.get(&peer) {
            if let Ok(result) = Self::poll_socket(socket).await {
                if let Some(decode_result) = result {
                    match decode_result {
                        Ok(frame) => {
                            trace!("Decoded {:?}", frame);
                            Self::handle_frame(frame, peer, &switch, &tree, &snek).await;
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
        socket: &Mutex<Framed<TcpStream, PineconeCodec>>,
    ) -> Result<Option<Result<Frame, std::io::Error>>, Elapsed> {
        let mut socket = socket.lock().await;
        let result = time::timeout(
            Duration::from_millis(10),
            async move { socket.next().await },
        )
        .await;
        result
    }

    async fn get_new_port(switch: &SwitchState) -> Port {
        for i in 1.. {
            let mut used = false;
            for port in switch.ports.read().await.values() {
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
    async fn disconnect_port(port: Port, switch: &SwitchState, tree: &TreeState, snek: &SnekState) {
        let peer = Self::get_peer_on_port(port, switch).await.unwrap();
        let mut bootstrap = false;
        // Scan the local DHT table for any routes that transited this now-dead
        // peering. If we find any then we need to send teardowns in the opposite
        // direction, so that nodes further along the path will learn that the
        // path was broken.
        for (key, value) in snek.paths.read().await.clone() {
            if value.destination == port || value.source == port {
                Self::send_teardown_for_existing_path(
                    port,
                    key.public_key,
                    key.path_id,
                    switch,
                    tree,
                    snek,
                )
                .await;
            }
        }

        // If the ascending path was also lost because it went via the now-dead
        // peering then clear that path (although we can't send a teardown) and
        // then bootstrap again.
        if let Some(asc) = &snek.ascending_path.read().await.clone() {
            let ascending = snek.paths.read().await.get(&asc).unwrap().clone();
            if ascending.destination == port {
                Self::teardown_path(0, asc.public_key, asc.path_id, snek).await;
                bootstrap = true;
            }
        }

        // If the descending path was lost because it went via the now-dead
        // peering then clear that path (although we can't send a teardown) and
        // wait for another incoming setup.
        if let Some(desc) = &snek.descending_path.read().await.clone() {
            let descending = snek.paths.read().await.get(&desc).unwrap().clone();
            if descending.destination == port {
                Self::teardown_path(0, desc.public_key, desc.path_id, snek).await;
            }
        }

        // If the peer that died was our chosen tree parent, then we will need to
        // select a new parent. If we successfully choose a new parent (as in, we
        // don't end up promoting ourselves to a root) then we will also need to
        // send a new bootstrap into the network.
        if Self::parent(tree).await == peer {
            bootstrap = bootstrap || Self::parent_selection(switch, tree).await;
        }

        if bootstrap {
            Self::bootstrap_now(switch, tree, snek).await;
        }
    }

    async fn tree_announcement(of: VerificationKey, tree: &TreeState) -> Option<TreeAnnouncement> {
        tree.announcements.read().await.get(&of).cloned()
    }
    async fn set_tree_announcement(
        of: VerificationKey,
        announcement: TreeAnnouncement,
        tree: &TreeState,
    ) {
        tree.announcements
            .write()
            .await
            .insert(of.clone(), announcement);
    }
    async fn port(of: VerificationKey, switch: &SwitchState) -> Option<Port> {
        if  Self::public_key(switch) == of {
            return Some(0);
        }
        let ports = switch.ports.read().await;
        ports.get(&of).cloned()
    }
    async fn peers(switch: &SwitchState) -> Vec<VerificationKey> {
        let mut peers = Vec::new();
        for (peer, port) in &*switch.ports.read().await {
            peers.push(peer.clone());
        }
        peers
    }
    async fn parent(tree: &TreeState) -> VerificationKey {
        *tree.parent.read().await
    }
    async fn set_parent(peer: VerificationKey, tree: &TreeState) {
        info!("Setting parent to {:?}", peer);
        *tree.parent.write().await = peer;
    }
    fn public_key(switch: &SwitchState) -> VerificationKey {
        switch.public_key
    }
    async fn get_peer_on_port(port: Port, switch: &SwitchState) -> Option<VerificationKey> {
        for (peer, peer_port) in &*switch.ports.read().await {
            if &port == peer_port {
                return Some(peer.clone());
            }
        }
        None
    }
    async fn current_sequence(tree: &TreeState) -> SequenceNumber {
        *tree.sequence.read().await
    }
    async fn next_sequence(tree: &TreeState) -> SequenceNumber {
        {
            let mut sequence = tree.sequence.write().await;
            *sequence += 1;
        }
        *tree.sequence.read().await
    }
    async fn current_ordering(tree: &TreeState) -> SequenceNumber {
        *tree.ordering.read().await
    }
    async fn next_ordering(tree: &TreeState) -> SequenceNumber {
        {
            let mut ordering = tree.ordering.write().await;
            *ordering += 1;
        }
        *tree.ordering.read().await
    }
    async fn reparent_timer_expired(tree: &TreeState) -> bool {
        let timer = tree.reparent_timer.read().await;
        if let Some(timer) = &*timer {
            timer.is_expired()
        } else {
            true
        }
    }
    async fn set_reparent_timer(tree: &TreeState) {
        trace!("Reparent in {:?}", REPARENT_WAIT_TIME);
        *tree.reparent_timer.write().await = Some(WaitTimer::new(REPARENT_WAIT_TIME));
    }
    async fn announcement_timer_expired(tree: &TreeState) -> bool {
        tree.announcement_timer.read().await.is_expired()
    }
    async fn reset_announcement_timer(tree: &TreeState) {
        *tree.announcement_timer.write().await = WaitTimer::new(ANNOUNCEMENT_INTERVAL);
    }
    async fn send_to_local(frame: Frame, switch: &SwitchState) {
        switch.download.send(frame).await;
    }
    async fn send(frame: Frame, to: VerificationKey, switch: &SwitchState) {
        let sockets = switch.sockets.read().await;
        let socket = sockets.get(&to);
        if let Some(socket) = socket {
            trace!("Sending {:?}", frame);
            let mut socket = socket.lock().await;
            socket.send(frame).await.unwrap();
        } else {
            debug!("No Socket for {:?}", to);
        }
    }

    async fn handle_frame(
        frame: Frame,
        from: VerificationKey,
        switch: &SwitchState,
        tree: &TreeState,
        snek: &SnekState,
    ) {
        trace!("Handling Frame...");
        match frame {
            Frame::TreeRouted(packet) => {
                trace!("Frame is TreeRouted");
                if let Some(peer) = Self::next_tree_hop(&packet, from, switch, tree).await {
                    let peer = peer;
                    if peer == Self::public_key(switch) {
                        Self::send_to_local(Frame::TreeRouted(packet), switch).await;
                        return;
                    }
                    Self::send(Frame::TreeRouted(packet), peer, switch).await;
                    return;
                }
                return;
            }
            Frame::SnekRouted(packet) => {
                trace!("Frame is SnekRouted");
                if let Some(peer) =
                    Self::next_snek_hop(&packet, false, true, switch, tree, snek).await
                {
                    let peer = peer;
                    if peer == Self::public_key(switch) {
                        Self::send_to_local(Frame::SnekRouted(packet), switch).await;
                        return;
                    }
                    Self::send(Frame::SnekRouted(packet), peer, switch).await;
                    return;
                }
                return;
            }
            Frame::TreeAnnouncement(announcement) => {
                trace!("Frame is TreeAnnouncement");
                Self::handle_tree_announcement(announcement, from, switch, tree, snek).await;
            }

            Frame::SnekBootstrap(bootstrap) => {
                Self::handle_snek_bootstrap(bootstrap, switch, tree).await;
            }
            Frame::SnekBootstrapACK(ack) => {
                Self::handle_snek_bootstrap_ack(ack, switch, tree, snek).await;
            }
            Frame::SnekSetup(setup) => {
                let from_port = Self::port(from, switch).await.unwrap().clone();
                let next_hop = Self::next_tree_hop(&setup, from, switch, tree)
                    .await
                    .unwrap();
                let next_hop_port = Self::port(next_hop, switch).await.unwrap().clone();
                Self::handle_setup(from_port, setup, next_hop_port, switch, tree, snek).await;
            }
            Frame::SnekSetupACK(ack) => {
                let port = Self::port(from, switch).await.unwrap().clone();
                Self::handle_setup_ack(port, ack, switch, tree, snek).await;
            }
            Frame::SnekTeardown(teardown) => {
                let port = Self::port(from, switch).await.unwrap().clone();
                Self::handle_teardown(port, teardown, snek).await;
            }
        }
    }
    async fn next_tree_hop(
        frame: &impl TreeRouted,
        from: VerificationKey,
        switch: &SwitchState,
        tree: &TreeState,
    ) -> Option<VerificationKey> {
        if frame.destination_coordinates() == Self::coordinates(switch, tree).await {
            return Some(Self::public_key(switch));
        }
        let our_distance = frame
            .destination_coordinates()
            .distance_to(&Self::coordinates(switch, tree).await);
        if our_distance == 0 {
            return Some(Self::public_key(switch));
        }

        let mut best_peer = None;
        let mut best_distance = our_distance;
        let mut best_ordering = SequenceNumber::MAX;
        for peer in Self::peers(switch).await {
            if peer == from {
                continue; // don't route back where the packet came from
            }
            if let None = Self::tree_announcement(peer, tree).await {
                continue; // ignore peers that haven't sent us announcements
            }
            if let Some(announcement) = Self::tree_announcement(peer, tree).await {
                if !(Self::current_root(switch, tree).await == announcement.root) {
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
    async fn handle_tree_announcement(
        mut frame: TreeAnnouncement,
        from: VerificationKey,
        switch: &SwitchState,
        tree: &TreeState,
        snek: &SnekState,
    ) {
        frame.receive_time = SystemTime::now();
        frame.receive_order = Self::next_ordering(tree).await;

        if let Some(announcement) = Self::tree_announcement(from, tree).await {
            if frame.has_same_root_key(&announcement) {
                if frame.replayed_old_sequence(&announcement) {
                    debug!("Announcement replayed old sequence. Dropping");
                    return;
                }
            }
        }
        trace!("Storing announcement {}", frame);
        Self::set_tree_announcement(from, frame.clone(), tree).await;
        if !Self::reparent_timer_expired(tree).await {
            debug!("Waiting to reparent");
            return;
        }
        if from == Self::parent(tree).await {
            trace!("Announcement came from parent");
            if frame.is_loop_of_child(&Self::public_key(switch)) {
                // SelectNewParentWithWait
                debug!("Announcement contains loop");
                Self::become_root(switch, tree).await;
                Self::reparent(true, switch, tree, snek).await;
                return;
            }
            if frame.root.public_key
                < Self::current_announcement(switch, tree)
                    .await
                    .root
                    .public_key
            {
                // SelectNewParentWithWait
                debug!("Announcement has weaker root");
                Self::become_root(switch, tree).await;
                Self::reparent(true, switch, tree, snek).await;
                return;
            }
            if frame.root.public_key
                > Self::current_announcement(switch, tree)
                    .await
                    .root
                    .public_key
            {
                // AcceptUpdate
                debug!("Announcement has stronger root. Forwarding to peers");
                Self::send_tree_announcements_to_all(
                    Self::current_announcement(switch, tree).await,
                    switch,
                )
                .await;
                return;
            }
            if frame.root.public_key
                == Self::current_announcement(switch, tree)
                    .await
                    .root
                    .public_key
            {
                if frame.root.sequence_number
                    > Self::current_announcement(switch, tree)
                        .await
                        .root
                        .sequence_number
                {
                    // AcceptUpdate
                    trace!("Announcement has higher sequence. Forwarding to peers");
                    Self::send_tree_announcements_to_all(
                        Self::current_announcement(switch, tree).await,
                        switch,
                    )
                    .await;
                    return;
                }
                // SelectNewParentWithWait
                debug!("Announcement replayed current sequence");
                Self::become_root(switch, tree).await;
                Self::reparent(true, switch, tree, snek).await;
                return;
            }
        } else {
            trace!("Announcement didn't come from parent");
            if frame.is_loop_of_child(&Self::public_key(switch)) {
                // DropFrame
                trace!("Announcement contains loop. Dropping");
                return;
            }
            if frame.root.public_key
                > Self::current_announcement(switch, tree)
                    .await
                    .root
                    .public_key
            {
                // AcceptNewParent
                trace!("Announcement has stronger root. Forwarding to peers");
                Self::set_parent(from.clone(), tree).await;
                let announcement = Self::current_announcement(switch, tree).await;
                Self::send_tree_announcements_to_all(announcement, switch).await;
                return;
            }
            if frame.root.public_key
                < Self::current_announcement(switch, tree)
                    .await
                    .root
                    .public_key
            {
                // InformPeerOfStrongerRoot
                trace!("Announcement has weaker root. Sending my announcement");
                Self::send_tree_announcement(
                    from,
                    Self::current_announcement(switch, tree).await,
                    switch,
                )
                .await;
                return;
            }
            if frame.root.public_key
                == Self::current_announcement(switch, tree)
                    .await
                    .root
                    .public_key
            {
                // SelectNewParent
                trace!("Announcement has same root");
                Self::reparent(false, switch, tree, snek).await;
                return;
            }
        }
    }
    async fn current_announcement(switch: &SwitchState, tree: &TreeState) -> TreeAnnouncement {
        if let Some(announcement) = Self::tree_announcement(Self::parent(tree).await, tree).await {
            announcement.clone()
        } else {
            TreeAnnouncement {
                root: Root {
                    public_key: Self::public_key(switch),
                    sequence_number: Self::current_sequence(tree).await,
                },
                signatures: vec![],
                receive_time: SystemTime::now(),
                receive_order: Self::current_ordering(tree).await,
            }
        }
    }
    async fn coordinates(switch: &SwitchState, tree: &TreeState) -> Coordinates {
        Self::current_announcement(switch, tree).await.coords()
    }
    async fn send_tree_announcements_to_all(announcement: TreeAnnouncement, switch: &SwitchState) {
        trace!("Sending tree announcements to all peers");
        for peer in Self::peers(switch).await {
            Self::send_tree_announcement(peer, announcement.clone(), switch).await;
        }
    }
    async fn send_tree_announcement(
        to: VerificationKey,
        announcement: TreeAnnouncement,
        switch: &SwitchState,
    ) {
        let port = Self::port(to, switch).await.unwrap();
        let signed_announcement = announcement.append_signature(switch.public_key, port);
        debug!("Sending tree announcement to port {}", port);
        Self::send(Frame::TreeAnnouncement(signed_announcement), to, switch).await;
    }
    async fn new_tree_announcement(switch: &SwitchState, tree: &TreeState) -> TreeAnnouncement {
        TreeAnnouncement {
            root: Root {
                public_key: Self::public_key(switch),
                sequence_number: Self::next_sequence(tree).await,
            },
            signatures: vec![],
            receive_time: SystemTime::now(),
            receive_order: 0,
        }
    }
    async fn parent_selection(switch: &SwitchState, tree: &TreeState) -> bool {
        trace!("Running parent selection...");
        if Self::public_key(switch) > Self::current_root(switch, tree).await.public_key {
            debug!("My key is stronger than current root");
            Self::become_root(switch, tree).await;
        }
        let mut best_root = Self::current_root(switch, tree).await;
        let mut best_peer = None;
        let mut best_order = SequenceNumber::MAX;
        for peer in Self::peers(switch).await {
            if let Some(announcement) = Self::tree_announcement(peer, tree).await {
                if announcement.receive_time.elapsed().unwrap() > ANNOUNCEMENT_TIMEOUT {
                    continue;
                }
                if announcement.is_loop_of_child(&Self::public_key(switch)) {
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
                if best_peer == Self::parent(tree).await {
                    debug!("Current parent is the best available parent");
                    return false;
                }
                let best_peer = best_peer.clone();
                Self::set_parent(best_peer, tree).await;
                Self::send_tree_announcements_to_all(
                    Self::current_announcement(switch, tree).await,
                    switch,
                )
                .await;
                return true;
            }
            None => {
                debug!("I am root");
                Self::become_root(switch, tree).await;
                return false;
            }
        }
    }
    async fn become_root(switch: &SwitchState, tree: &TreeState) {
        trace!("Becoming root");
        Self::set_parent(Self::public_key(switch).clone(), tree).await;
    }
    async fn reparent(wait: bool, switch: &SwitchState, tree: &TreeState, snek: &SnekState) {
        let switch = switch.clone();
        let tree = tree.clone();
        let snek = snek.clone();
        tokio::spawn(async move {
            if wait {
                trace!("Waiting to reparent");
                sleep(REPARENT_WAIT_TIME).await;
            }
            trace!("Re-parenting");
            if Self::parent_selection(&switch, &tree).await {
                Self::bootstrap_now(&switch, &tree, &snek).await;
            }
        });
    }
    async fn current_root(switch: &SwitchState, tree: &TreeState) -> Root {
        Self::current_announcement(switch, tree).await.root
    }
    async fn maintain_tree(wait: bool, switch: &SwitchState, tree: &TreeState, snek: &SnekState) {
        if Self::i_am_root(switch, tree).await {
            if Self::announcement_timer_expired(tree).await && wait {
                Self::reset_announcement_timer(tree).await;
                let announcement = Self::new_tree_announcement(switch, tree).await;
                Self::send_tree_announcements_to_all(announcement, switch).await;
            }
        }
        Self::reparent(true, switch, tree, snek).await;
    }
    async fn i_am_root(switch: &SwitchState, tree: &TreeState) -> bool {
        Self::public_key(switch) == Self::parent(tree).await
    }

    /// `maintain_snake` is responsible for working out if we need to send bootstraps
    /// or to clean up any old paths.
    async fn maintain_snek(switch: &SwitchState, tree: &TreeState, snek: &SnekState) {
        // Work out if we are able to bootstrap. If we are the root node then
        // we don't send bootstraps, since there's nowhere for them to go —
        // bootstraps are sent up to the next ascending node, but as the root,
        // we already have the highest key on the network.
        let root_announcement = Self::current_announcement(switch, tree).await;
        let can_bootstrap = Self::parent(tree).await != Self::public_key(switch)
            && root_announcement.root.public_key != Self::public_key(switch);
        let mut will_bootstrap = false;

        // The ascending node is the node with the next highest key.
        if let Some(asc) = &snek.ascending_path.read().await.clone() {
            let ascending = snek.paths.read().await.get(&asc).unwrap().clone();
            if !ascending.valid() {
                // The ascending path entry has expired, so tear it down and then
                // see if we can bootstrap again.
                Self::send_teardown_for_existing_path(
                    0,
                    asc.public_key,
                    asc.path_id,
                    switch,
                    tree,
                    snek,
                )
                .await;
            }
            if ascending.root == root_announcement.root {
                // The ascending node was set up with a different root key or sequence
                // number. In this case, we will send another bootstrap to the remote
                // side in order to hopefully replace the path with a new one.
                will_bootstrap = can_bootstrap;
            }
        } else {
            // We don't have an ascending node at all, so if we can, we'll try
            // bootstrapping to locate it.
            will_bootstrap = can_bootstrap;
        }

        // The descending node is the node with the next lowest key.
        if let Some(desc) = &snek.descending_path.read().await.clone() {
            let descending_path = snek.paths.read().await.get(&desc).unwrap().clone();
            if !descending_path.valid() {
                // The descending path has expired, so tear it down and then that should
                // prompt the remote side into sending a new bootstrap to set up a new
                // path, if they are still alive.
                Self::send_teardown_for_existing_path(
                    0,
                    desc.public_key,
                    desc.path_id,
                    switch,
                    tree,
                    snek,
                )
                .await;
            }
        }

        // Clean up any paths that were installed more than 5 seconds ago but haven't
        // been activated by a setup ACK.
        for (index, path) in snek.paths.read().await.clone() {
            if !path.active && path.last_seen.elapsed().unwrap() > Duration::from_secs(5) {
                Self::send_teardown_for_existing_path(
                    0,
                    index.public_key,
                    index.path_id,
                    switch,
                    tree,
                    snek,
                )
                .await;
            }
        }

        // If one of the previous conditions means that we need to bootstrap, then
        // send the actual bootstrap message into the network.
        if will_bootstrap {
            Self::bootstrap_now(switch, tree, snek).await;
        }
    }

    /// `bootstrap_now` is responsible for sending a bootstrap massage to the network
    async fn bootstrap_now(switch: &SwitchState, tree: &TreeState, snek: &SnekState) {
        trace!("Bootstrapping ...");
        // If we are the root node then there's no point in trying to bootstrap. We
        // already have the highest public key on the network so a bootstrap won't be
        // able to go anywhere in ascending order.
        if Self::parent(tree).await == Self::public_key(switch) {
            trace!("Not bootstrapping because I am root");
            return;
        }

        // If we already have a relationship with an ascending node and that has the
        // same root key and sequence number (i.e. nothing has changed in the tree since
        // the path was set up) then we don't need to send another bootstrap message just
        // yet. We'll either wait for the path to be torn down, expire or for the tree to
        // change.
        let announcement = Self::current_announcement(switch, tree).await;
        if let Some(asc) = &*snek.ascending_path.read().await {
            let paths = snek.paths.read().await;
            let ascending = paths.get(&asc).unwrap();
            let asc_peer = Self::get_peer_on_port(ascending.source, switch)
                .await
                .unwrap();
            if ascending.root == announcement.root {
                trace!("Not bootstrapping because a valid ascending path is set");
                return;
            }
        }

        // Construct the bootstrap packet. We will include our root key and sequence
        // number in the update so that the remote side can determine if we are both using
        // the same root node when processing the update.
        let frame = SnekBootstrap {
            root: Self::current_root(switch, tree).await,
            destination_key: Self::public_key(switch),
            source: Self::coordinates(switch, tree).await,
            path_id: thread_rng().gen(),
        };

        if let Some(peer) = Self::next_snek_hop(&frame, true, false, switch, tree, snek).await {
            trace!("Bootstrapping path {} ", frame.path_id);
            Self::send(Frame::SnekBootstrap(frame), peer, switch).await;
        } else {
            trace!("Not bootstrapping because no next hop was found");
        }
    }

    async fn next_snek_hop(
        frame: &impl SnekRouted,
        bootstrap: bool,
        traffic: bool,
        switch: &SwitchState,
        tree: &TreeState,
        snek: &SnekState,
    ) -> Option<VerificationKey> {
        let destination_key = frame.destination_key();
        // If the message isn't a bootstrap message and the destination is for our
        // own public key, handle the frame locally — it's basically loopback.
        if !bootstrap && Self::public_key(switch) == destination_key {
            return Some(Self::public_key(switch));
        }

        // We start off with our own key as the best key. Any suitable next-hop
        // candidate has to improve on our own key in order to forward the frame.
        let mut best_peer = None;
        if !traffic {
            best_peer = Some(Self::public_key(switch));
        }
        let mut best_key = Self::public_key(switch);

        // Check if we can use the path to the root via our parent as a starting
        // point. We can't do this if we are the root node as there would be no
        // parent or ascending paths.
        if Self::parent(tree).await != Self::public_key(switch) {
            if bootstrap && best_key == destination_key {
                // Bootstraps always start working towards their root so that they
                // go somewhere rather than getting stuck.
                best_key = Self::current_announcement(switch, tree)
                    .await
                    .root
                    .public_key;
                best_peer = Some(Self::parent(tree).await)
            }
            if Self::dht_ordered(
                &best_key,
                &destination_key,
                &Self::current_announcement(switch, tree)
                    .await
                    .root
                    .public_key,
            ) {
                // The destination key is higher than our own key, so start using
                // the path to the root as the first candidate.
                best_key = Self::current_announcement(switch, tree)
                    .await
                    .root
                    .public_key;
                best_peer = Some(Self::parent(tree).await)
            }

            // Check our direct ancestors in the tree, that is, all nodes between
            // ourselves and the root node via the parent port.
            for ancestor in Self::current_announcement(switch, tree)
                .await
                .signatures
                .iter()
                .map(|x| x.signing_public_key)
            {
                if !bootstrap && ancestor == destination_key && best_key != destination_key {
                    best_key = ancestor;
                    best_peer = Some(Self::parent(tree).await);
                }
                if Self::dht_ordered(&destination_key, &ancestor, &best_key) {
                    best_key = ancestor;
                    best_peer = Some(Self::parent(tree).await);
                }
            }
        }

        // Check all of the ancestors of our direct peers too, that is, all nodes
        // between our direct peer and the root node.
        for (peer, announcement) in &*tree.announcements.read().await {
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
        for peer in Self::peers(switch).await {
            if best_key == peer {
                best_key = peer;
                best_peer = Some(peer);
            }
        }

        // Check our DHT entries. In particular, we are only looking at the source
        // side of the DHT paths. Since setups travel from the lower key to the
        // higher one, this is effectively looking for paths that descend through
        // keyspace toward lower keys rather than ascend toward higher ones.
        for (key, entry) in &*snek.paths.read().await {
            if !entry.valid() || entry.source == 0 {
                continue;
            }
            if !bootstrap && !entry.active {
                continue;
            }
            if !bootstrap && key.public_key == destination_key && best_key != destination_key {
                best_key = key.public_key;
                best_peer = Some(Self::get_peer_on_port(entry.source, switch).await.unwrap());
            }
            if Self::dht_ordered(&destination_key, &key.public_key, &best_key) {
                best_key = key.public_key;
                best_peer = Some(Self::get_peer_on_port(entry.source, switch).await.unwrap());
            }
        }
        best_peer
    }

    /// `handle_bootstrap` is called in response to receiving a bootstrap packet.
    /// This function will send a bootstrap ACK back to the sender.
    async fn handle_snek_bootstrap(frame: SnekBootstrap, switch: &SwitchState, tree: &TreeState) {
        // Check that the root key and sequence number in the update match our
        // current root, otherwise we won't be able to route back to them using
        // tree routing anyway. If they don't match, silently drop the bootstrap.
        if Self::current_root(switch, tree).await == frame.root {
            trace!("Handling Bootstrap...");
            // In response to a bootstrap, we'll send back a bootstrap ACK packet to
            // the sender. We'll include our own root details in the ACK.
            let frame = SnekBootstrapAck {
                // Bootstrap ACKs are routed using tree routing, so we need to take the
                // coordinates from the source field of the received packet and set the
                // destination of the ACK packet to that.
                destination_coordinates: frame.source.clone(),
                destination_key: frame.destination_key,
                source_coordinates: Self::coordinates(switch, tree).await,
                source_key: Self::public_key(switch),
                root: Self::current_root(switch, tree).await,
                path_id: frame.path_id,
            };
            if let Some(peer) =
                Self::next_tree_hop(&frame, Self::public_key(switch), switch, tree).await
            {
                Self::send(Frame::SnekBootstrapACK(frame), peer, switch).await;
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
    async fn handle_snek_bootstrap_ack(
        ack: SnekBootstrapAck,
        switch: &SwitchState,
        tree: &TreeState,
        snek: &SnekState,
    ) {
        let ascending_path = snek.ascending_path.read().await;
        let mut paths = snek.paths.write().await;
        let mut update = false;
        if ack.source_key == Self::public_key(switch) {
            // We received a bootstrap ACK from ourselves. This shouldn't happen,
            // so either another node has forwarded it to us incorrectly, or
            // a routing loop has occurred somewhere. Don't act on the bootstrap
            // in that case.
        } else if ack.root != Self::current_root(switch, tree).await {
            // The root key in the bootstrap ACK doesn't match our own key, or the
            // sequence doesn't match, so it is quite possible that routing setup packets
            // using tree routing would fail.
        } else if let Some(asc) = &*ascending_path {
            if let Some(ascending) = paths.get(&asc) {
                if ascending.valid() {
                    // We already have an ascending entry and it hasn't expired yet.
                    if ascending.origin == ack.source_key && ack.path_id != asc.path_id {
                        // We've received another bootstrap ACK from our direct ascending node.
                        // Just refresh the record and then send a new path setup message to
                        // that node.
                        update = true
                    } else if Self::dht_ordered(
                        &Self::public_key(switch),
                        &ack.source_key,
                        &ascending.origin,
                    ) {
                        // We know about an ascending node already but it turns out that this
                        // new node that we've received a bootstrap from is actually closer to
                        // us than the previous node. We'll update our record to use the new
                        // node instead and then send a new path setup message to it.
                        update = true;
                    }
                } else {
                    // Ascending Path expired.
                    if Self::public_key(switch) < ack.source_key {
                        // We don't know about an ascending node and at the moment we don't know
                        // any better candidates, so we'll accept a bootstrap ACK from a node with a
                        // key higher than ours (so that it matches descending order).
                        update = true;
                    }
                }
            }
        } else if None == *ascending_path {
            // We don't have an ascending entry
            if Self::public_key(switch) < ack.source_key {
                // We don't know about an ascending node and at the moment we don't know
                // any better candidates, so we'll accept a bootstrap ACK from a node with a
                // key higher than ours (so that it matches descending order).
                update = true;
            }
        } else {
            // The bootstrap ACK conditions weren't met. This might just be because
            // there's a node out there that hasn't converged to a closer node
            // yet, so we'll just ignore the acknowledgement.
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
            root: Self::current_root(switch, tree).await,
            destination: ack.source_coordinates,
            destination_key: ack.source_key.clone(),
            source_key: Self::public_key(switch),
            path_id: ack.path_id,
        };
        let next_hop = Self::next_tree_hop(&setup, Self::public_key(switch), switch, tree).await;

        // Importantly, we will only create a DHT entry if it appears as though our next
        // hop has actually accepted the packet. Otherwise we'll create a path entry and
        // the setup message won't go anywhere.
        match next_hop {
            None => {
                // No peer was identified, which shouldn't happen.
                return;
            }
            Some(next_peer) => {
                if Self::public_key(switch) == next_peer {
                    // The peer is local, which shouldn't happen.
                    return;
                }
                Self::send(Frame::SnekSetup(setup), next_peer, switch).await;
                let index = SnekPathIndex {
                    public_key: Self::public_key(switch),
                    path_id: ack.path_id.clone(),
                };
                let entry = SnekPath {
                    origin: ack.source_key,
                    target: ack.source_key,
                    source: 0,
                    destination: Self::port(next_peer, switch).await.unwrap().clone(),
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
                        Self::send_teardown_for_existing_path(
                            0,
                            dht_key.public_key,
                            dht_key.path_id,
                            switch,
                            tree,
                            snek,
                        )
                        .await;
                    }
                }
                // Install the new route into the DHT.
                paths.insert(index, entry.clone());
                *snek.candidate.write().await = Some(entry);
            }
        }
    }

    /// `handle_setup` is called in response to receiving setup packets. Note that
    /// these packets are handled even as we forward them, as setup packets should be
    /// processed by each node on the path.
    async fn handle_setup(
        from: Port,
        rx: SnekSetup,
        next_hop: Port,
        switch: &SwitchState,
        tree: &TreeState,
        snek: &SnekState,
    ) {
        let mut descending_path = snek.ascending_path.write().await;
        let mut paths = snek.paths.write().await;
        if Self::current_root(switch, tree).await != rx.root {
            Self::send_teardown_for_rejected_path(rx.source_key, rx.path_id, from, switch, tree)
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
            Self::send_teardown_for_existing_path(0, rx.source_key, rx.path_id, switch, tree, snek)
                .await;
            Self::send_teardown_for_rejected_path(rx.source_key, rx.path_id, from, switch, tree)
                .await;
            return;
        }
        // If we're at the destination of the setup then update our predecessor
        // with information from the bootstrap.
        if rx.destination_key == Self::public_key(switch) {
            let mut update = false;
            if Self::current_root(switch, tree).await == rx.root {
                // The root key in the bootstrap ACK doesn't match our own key, or the
                // sequence doesn't match, so it is quite possible that routing setup packets
                // using tree routing would fail.
            } else if rx.source_key < Self::public_key(switch) {
                // The bootstrapping key should be less than ours but it isn't.
            } else if let Some(desc) = &*descending_path {
                let descending = paths.get(desc).unwrap();
                if descending.valid() {
                    // We already have a descending entry and it hasn't expired.
                    if desc.public_key == rx.source_key && rx.path_id != desc.path_id {
                        // We've received another bootstrap from our direct descending node.
                        // Send back an acknowledgement as this is OK.
                        update = true;
                    } else if Self::dht_ordered(
                        &desc.public_key,
                        &rx.source_key,
                        &Self::public_key(switch),
                    ) {
                        // The bootstrapping node is closer to us than our previous descending
                        // node was.
                        update = true;
                    } else {
                        // Our descending entry has expired
                        if rx.source_key < Self::public_key(switch) {
                            // The bootstrapping key is less than ours so we'll acknowledge it.
                            update = true;
                        }
                    }
                }
            } else if let None = *descending_path {
                // We don't have a descending entry
                if rx.source_key < Self::public_key(switch) {
                    // The bootstrapping key is less than ours so we'll acknowledge it.
                    update = true;
                }
            } else {
                // The bootstrap conditions weren't met. This might just be because
                // there's a node out there that hasn't converged to a closer node
                // yet, so we'll just ignore the bootstrap.
            }
            if !update {
                Self::send_teardown_for_rejected_path(
                    rx.source_key,
                    rx.path_id,
                    from,
                    switch,
                    tree,
                )
                .await;
                return;
            }
            if let Some(previous_path) = &descending_path.clone() {
                Self::send_teardown_for_existing_path(
                    0,
                    previous_path.public_key,
                    previous_path.path_id,
                    switch,
                    tree,
                    snek,
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
            Self::send(
                Frame::SnekSetupACK(setup_ack),
                Self::get_peer_on_port(entry.source, switch).await.unwrap(),
                switch,
            )
            .await;
            return;
        }

        // Try to forward the setup onto the next node first. If we
        // can't do that then there's no point in keeping the path.
        let next_peer = Self::get_peer_on_port(next_hop, switch).await.unwrap();
        if next_peer == Self::public_key(switch) {
            debug!("Next hop for {:?} is local, which shouldn't happen.", rx);
            Self::send_teardown_for_rejected_path(rx.source_key, rx.path_id, from, switch, tree)
                .await;
            return;
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
    async fn handle_setup_ack(
        from: Port,
        rx: SnekSetupAck,
        switch: &SwitchState,
        tree: &TreeState,
        snek: &SnekState,
    ) {
        let mut paths = snek.paths.write().await;
        // Look up to see if we have a matching route. The route must be not active
        // (i.e. we haven't received a setup ACK for it yet) and must have arrived
        // from the port that the entry was populated with.
        for (key, entry) in paths.clone() {
            if entry.active || key.public_key != rx.destination_key || key.path_id != rx.path_id {
                continue;
            }
            if from == 0 {}
            if from == entry.destination {
                if entry.source == 0 {
                    let entry_source = Self::get_peer_on_port(entry.source, switch).await.unwrap();
                    Self::send(Frame::SnekSetupACK(rx.clone()), entry_source, switch).await;
                }
            }
        }
        for (key, entry) in paths.iter_mut() {
            if entry.active || key.public_key != rx.destination_key || key.path_id != rx.path_id {
                continue;
            }
            if from == 0 {}
            if from == entry.destination {
                if entry.source == 0 {
                    entry.active = true;
                    if let Some(candidate) = &snek.candidate.read().await.clone() {
                        if entry == candidate {
                            *snek.candidate.write().await = None;
                        }
                    }
                }
            }
        }
    }

    /// `handle_teardown` is called in response to receiving a teardown
    /// packet from the network
    async fn handle_teardown(from: Port, rx: SnekTeardown, snek: &SnekState) -> Vec<Port> {
        Self::teardown_path(from, rx.destination_key, rx.path_id, snek).await
    }

    /// `teardown_path` processes a teardown message by tearing down any
    /// related routes, returning a slice of next-hop candidates that the
    /// teardown must be forwarded to.
    async fn teardown_path(
        from: Port,
        path_key: VerificationKey,
        path_id: SnekPathId,
        snek: &SnekState,
    ) -> Vec<Port> {
        let mut ascending_path = snek.ascending_path.write().await;
        let mut descending_path = snek.descending_path.write().await;
        let mut paths = snek.paths.write().await;
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
        from: Port,
        path_key: VerificationKey,
        path_id: SnekPathId,
        switch: &SwitchState,
        tree: &TreeState,
        snek: &SnekState,
    ) {
        let frame = Self::get_teardown(path_key, path_id, switch, tree).await;
        for next_hop in Self::teardown_path(from, path_key, path_id, snek).await {
            let peer = Self::get_peer_on_port(next_hop, switch).await.unwrap();
            Self::send(Frame::SnekTeardown(frame.clone()), peer, switch).await;
        }
    }
    async fn send_teardown_for_rejected_path(
        path_key: VerificationKey,
        path_id: SnekPathId,
        via: Port,
        switch: &SwitchState,
        tree: &TreeState,
    ) {
        let frame = Self::get_teardown(path_key, path_id, switch, tree).await;
        let peer = Self::get_peer_on_port(via, switch).await.unwrap();
        Self::send(Frame::SnekTeardown(frame), peer, switch).await;
    }

    async fn get_teardown(
        path_key: VerificationKey,
        path_id: SnekPathId,
        switch: &SwitchState,
        tree: &TreeState,
    ) -> SnekTeardown {
        SnekTeardown {
            root: Self::current_root(switch, tree).await,
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
