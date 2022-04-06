use std::collections::HashMap;
use std::io::Error;
use futures::SinkExt;
use crate::frames::Frame;
use crate::{PineconeCodec, TreeAnnouncement};
use log::{debug, info, trace};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::sync::mpsc::{Receiver, Sender};
use tokio::sync::{Mutex, RwLock};
use tokio::task::JoinHandle;
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
    SendFrameToPeer{frame: Frame, to: VerificationKey},
    RemovePeer(VerificationKey),
    StopRouter,
}
struct SwitchState {
    pub(crate) public_key: VerificationKey,
    pub(crate) sockets: RwLock<HashMap<VerificationKey, Mutex<Framed<TcpStream, PineconeCodec>>>>,
    ports: RwLock<HashMap<VerificationKey, Port>>,
}
struct TreeState {
    parent: RwLock<VerificationKey>,
    announcements: RwLock<HashMap<VerificationKey, TreeAnnouncement>>,
    sequence: RwLock<SequenceNumber>,
    ordering: RwLock<SequenceNumber>,
}
pub struct Router {
    peer_handles: Mutex<Vec<JoinHandle<()>>>,

    pub(crate) upload: Receiver<Event>,
    pub(crate) download: Arc<Sender<Frame>>,

    switch: Arc<SwitchState>,
    tree: Arc<TreeState>,
}
impl Router {
    pub fn new(key: VerificationKey, download: Sender<Frame>, upload: Receiver<Event>) -> Self {
        Self {
            peer_handles: Default::default(),
            upload,
            download: Arc::new(download),
            switch: Arc::new(SwitchState {
                public_key: key,
                sockets: Default::default(),
                ports: Default::default()
            }),
            tree: Arc::new(TreeState {
                parent: RwLock::new(key),
                announcements: Default::default(),
                sequence: RwLock::new(0),
                ordering: RwLock::new(0)
            })
        }
    }
    pub fn spawn(mut self) -> JoinHandle<Self>{
        tokio::spawn(async move {
            loop {
                if let Some(event) = self.upload.recv().await {
                    match event {
                        Event::AddPeer { key, socket } => {
                            self.add_peer(key, socket).await;
                        }
                        Event::HandleFrame(frame) => {
                            Self::handle_frame(frame);
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
                            self.stop_router().await;
                            break;
                        }
                    }
                } else {
                    debug!("Local event channel closed. Stopping router");
                    self.stop_router().await;
                    break;
                }
            }
            self
        })
    }
    async fn stop_router(&self) {
        let mut handles = self.peer_handles.lock().await;
        for handle in handles.pop() {
            handle.await;
        }
    }
    async fn add_peer(&self, peer: VerificationKey, socket: Framed<TcpStream, PineconeCodec>) {
        let mut sockets = self.switch.sockets.write().await;
        if sockets.contains_key(&peer) {
            info!("Couldn't add {:?} because it already exists", peer);
            return;
        }
        sockets.insert(peer, Mutex::new(socket));
        info!("Added peer {:?}", peer);
        self.spawn_peer(peer).await;
    }
    async fn spawn_peer(&self, peer: VerificationKey) {
        let switch = self.switch.clone();
        self.peer_handles.lock().await.push(tokio::spawn(async move {
            loop {
                let sockets = switch.sockets.read().await;
                if let Some(socket) = sockets.get(&peer) {
                    let mut socket = socket.lock().await;
                    if let Some(result) = socket.next().await {
                        match result {
                            Ok(frame) => {
                                trace!("Decoded {:?}", frame);
                                Self::handle_frame(frame);
                            }
                            Err(e) => {
                                debug!("Could not decode {:?}", e);
                            }
                        }
                    } else {
                        // debug!("Stream of {:?} ended. Stopping peer", peer);
                        // break
                    }
                } else {
                    debug!("No stream for {:?}. Stopping peer", peer);
                    break
                }
            }
        }));
    }
    async fn socket_of_peer() {}
    async fn send_to_socket(socket: &mut Framed<TcpStream, PineconeCodec>, frame: Frame) {
        socket.send(frame).await.unwrap();
    }
    fn handle_frame(frame: Frame) {

    }
}
