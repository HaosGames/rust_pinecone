use crate::error::RouterError;
use crate::frames::Frame;
use crate::router::{PublicKey, Router};
use crate::session::Session;
use log::{debug, trace};
use std::collections::HashMap;
use std::sync::Arc;
use ed25519_consensus::SigningKey;
use futures_sink::Sink;
use tokio::sync::mpsc::{channel, Sender};
use tokio::sync::RwLock;
use tokio_stream::Stream;
#[cfg(doc)]
use tokio_util::codec::{FramedRead, FramedWrite};
#[cfg(doc)]
use tokio::io::{AsyncRead, AsyncWrite};
#[cfg(doc)]
use crate::wire_frame::PineconeCodec;
/// This is the object that is used to connect the router with other peers
/// and get [`Session`]s with other nodes in the overlay network.
///
/// Cloning this struct still gives access to the original router
/// that was created with the `new` method.
#[derive(Clone)]
pub struct Client {
    router_key: PublicKey,
    router: Router,
    upload: Sender<Frame>,
    session_senders: Arc<RwLock<HashMap<PublicKey, Sender<Frame>>>>,
}
#[allow(unused)]
impl Client {
    /// Creates a pinecone router and spawns various tokio tasks for it.
    pub async fn new(key: SigningKey) -> Self {
        let public_key = key.verification_key().to_bytes();
        let (upload_sender, upload_receiver) = channel(100);
        let (download_sender, mut download_receiver) = channel(100);
        let client = Self {
            router_key: public_key,
            router: Router::new(public_key, download_sender, upload_receiver),
            upload: upload_sender,
            session_senders: Arc::new(Default::default()),
        };
        let client1 = client.clone();
        tokio::spawn(async move {
            loop {
                let recv = download_receiver.recv().await;
                match recv {
                    None => {
                        break;
                    }
                    Some(frame) => match &frame {
                        Frame::SnekRouted(packet) => {
                            let senders = client1.session_senders.read().await;
                            if let Some(sender) = senders.get(&packet.source_key) {
                                let public_key = packet.source_key;
                                if let Err(e) = sender.send(frame).await {
                                    debug!(
                                        "Session upload channel of {:?} closed. Removing sender.",
                                        public_key
                                    );
                                    drop(senders);
                                    client1.session_senders.write().await.remove(&public_key);
                                }
                            } else {
                                trace!("No session for {:?}", packet.source_key);
                            }
                        }
                        e => {
                            trace!("Received protocol frame on client download channel {:?}", e);
                        }
                    },
                }
            }
            debug!("Stopped client download loop");
        });
        client.router.start().await;
        client
    }
    pub async fn stop(&self) {
        self.router.stop().await;
    }
    /// Directly connects a peer with the router using the given connections.
    ///
    /// `upload` is suggested to be a boxed [`FramedWrite`]<impl [`AsyncWrite`],[`PineconeCodec`]>
    ///
    /// `download` is suggested to be a boxed [`FramedRead`]<impl [`AsyncRead`],[`PineconeCodec`]>
    pub async fn connect_peer(
        &self,
        upload: Box<dyn Sink<Frame, Error = RouterError> + Send + Unpin>,
        download: Box<dyn Stream<Item = Result<Frame, RouterError>> + Send + Unpin>,
    ) -> Result<PublicKey, RouterError> {
        self.router.connect(upload, download).await
    }
    pub async fn disconnect_peer(&self, peer_key: PublicKey) {
        self.router.disconnect_peer(peer_key).await;
    }
    /// Dials a node with the given public key in the network and creates a session with it.
    /// This doesn't communicate with the actual node so the session is created
    /// regardless of wether this node is actually reachable or not.
    pub async fn dial(&self, public_key: PublicKey) -> Session {
        let (download_sender, download_receiver) = channel(100);
        self.session_senders
            .write()
            .await
            .insert(public_key, download_sender);
        Session {
            router_key: self.router_key,
            dialed_key: public_key,
            upload: self.upload.clone(),
            download: download_receiver,
        }
    }
}
