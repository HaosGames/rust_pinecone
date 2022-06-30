use crate::error::RouterError;
use crate::frames::Frame;
use crate::router::{PublicKey, Router};
use crate::session::{SendSession, Session};
#[cfg(doc)]
use crate::wire_frame::PineconeCodec;
use ed25519_consensus::SigningKey;
use futures_sink::Sink;
use log::{debug, trace};
use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
#[cfg(doc)]
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::mpsc::{channel, Receiver, Sender};
use tokio::sync::RwLock;
use tokio_stream::Stream;
#[cfg(doc)]
use tokio_util::codec::{FramedRead, FramedWrite};
/// This is the object that is used to connect the router with other peers
/// and get Sessions with other nodes in the overlay network.
///
/// Cloning this struct still gives access to the original router
/// that was created with the `new` method.
#[derive(Clone)]
pub struct Client {
    router_key: PublicKey,
    router: Router,
    upload: Sender<Frame>,
    session_senders: Arc<RwLock<HashMap<PublicKey, Sender<Frame>>>>,
    new_incoming: Arc<Sender<Session>>,
}
#[allow(unused)]
impl Client {
    /// Creates a pinecone router and spawns various tokio tasks for it.
    pub async fn new(key: SigningKey) -> (Self, Listener) {
        let public_key = key.verification_key().to_bytes();
        let (upload_sender, upload_receiver) = channel(100);
        let (download_sender, mut download_receiver) = channel(100);
        let (new_incoming_sender, new_incoming_receiver) = channel(100);
        let client = Self {
            router_key: public_key,
            router: Router::new(key, download_sender, upload_receiver),
            upload: upload_sender,
            session_senders: Arc::new(Default::default()),
            new_incoming: Arc::new(new_incoming_sender),
        };
        let client1 = client.clone();
        tokio::spawn(async move {
            loop {
                let recv = download_receiver.recv().await;
                match recv {
                    None => {
                        trace!("Client download sender was dropped.");
                        break;
                    }
                    Some(frame) => match &frame {
                        Frame::SnekRouted(packet) => {
                            let senders = client1.session_senders.read().await;
                            if let Some(sender) = senders.get(&packet.source_key) {
                                let public_key = packet.source_key;
                                if let Err(e) = sender.send(frame).await {
                                    debug!(
                                        "ReceiveSession for {:?} was closed. Removing sender",
                                        public_key
                                    );
                                    drop(senders);
                                    client1.session_senders.write().await.remove(&public_key);
                                }
                            } else {
                                trace!("No session for {:?}. Creating new one", packet.source_key);
                                let (download_sender, download_receiver) = channel(100);
                                client1
                                    .session_senders
                                    .write()
                                    .await
                                    .insert(packet.source_key, download_sender);
                                client1.new_incoming.send(Session {
                                    router_key: client1.router_key,
                                    dialed_key: packet.source_key,
                                    download: download_receiver,
                                    upload: client1.upload.clone(),
                                });
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
        (
            client,
            Listener {
                pending: new_incoming_receiver,
            },
        )
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
    /// Dials a node with the given public key in the network and creates a [`SendSession`] for it.
    /// This doesn't communicate with the actual node so the session is created
    /// regardless of weather this node is actually reachable or not.
    ///
    /// SendSessions can be created multiple times for a given key.
    pub async fn dial_send(&self, public_key: PublicKey) -> SendSession {
        SendSession {
            router_key: self.router_key,
            dialed_key: public_key,
            upload: self.upload.clone(),
        }
    }
    /// Dials a node with the given public key in the network and creates a [`Session`] for it.
    /// This doesn't communicate with the actual node so the session is created
    /// regardless of weather this node is actually reachable or not.
    ///
    /// Sessions can be created only once for a given key.
    pub async fn dial(&self, public_key: PublicKey) -> Result<Session, RouterError> {
        if self.session_senders.read().await.contains_key(&public_key) {
            return Err(RouterError::SessionAlreadyExists);
        }
        let (download_sender, download_receiver) = channel(100);
        self.session_senders
            .write()
            .await
            .insert(public_key, download_sender);
        Ok(Session {
            router_key: self.router_key,
            dialed_key: public_key,
            download: download_receiver,
            upload: self.upload.clone(),
        })
    }
}
/// Stream of new incoming [`ReceiveSession`]s.
///
/// Handed out when creating a new Client.
pub struct Listener {
    pending: Receiver<Session>,
}
impl Stream for Listener {
    type Item = Session;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match self.get_mut().pending.poll_recv(cx) {
            Poll::Ready(result) => {
                if let Some(session) = result {
                    Poll::Ready(Some(session))
                } else {
                    Poll::Ready(None)
                }
            }
            Poll::Pending => Poll::Pending,
        }
    }
}
