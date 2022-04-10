use crate::connection::{DownloadConnection, UploadConnection};
use crate::frames::{Frame, SnekPacket, TreeAnnouncement};
use crate::router::{Public_key, Router};
use crate::tree::Root;
use crate::wire_frame::PineconeCodec;
use ed25519_consensus::{SigningKey, VerificationKeyBytes};
use env_logger::WriteStyle;
use log::{debug, info, trace, LevelFilter};
use rand::thread_rng;
use std::env::args;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc::channel;
use tokio_util::codec::{FramedRead, FramedWrite};

mod connection;
mod coordinates;
mod frames;
mod router;
mod snek;
mod tree;
mod wait_timer;
mod wire_frame;

#[tokio::main]
async fn main() {
    let _ = env_logger::builder()
        .write_style(WriteStyle::Always)
        .format_timestamp(None)
        .filter_level(LevelFilter::Debug)
        .filter_module("rust_pinecone", LevelFilter::Trace)
        .init();
    let mode = args().nth(1).unwrap();
    match mode.as_str() {
        "listen" => {
            let signing_key = SigningKey::from([0; 32]);
            let verification_key = signing_key.verification_key();
            let public_key = verification_key.to_bytes();
            let (upload_sender, upload_receiver) = channel(100);
            let (download_sender, download_receiver) = channel(100);
            let router = Router::new(public_key, download_sender, upload_receiver);
            let handle = router.start().await;
            info!(
                "Router {}",
                serde_json::to_string(&VerificationKeyBytes::from(verification_key)).unwrap()
            );

            let listen_addr = args().nth(2).unwrap();
            info!("Listening on {}", listen_addr);
            let listener = TcpListener::bind(listen_addr).await.unwrap();
            let router1 = router.clone();
            tokio::spawn(async move {
                loop {
                    let (socket, addr) = listener.accept().await.unwrap();
                    let (reader, writer) = socket.into_split();
                    info!("New Client: {:?}", addr);
                    router1
                        .accept_peer(
                            UploadConnection::Tcp(FramedWrite::new(writer, PineconeCodec)),
                            DownloadConnection::Tcp(FramedRead::new(reader, PineconeCodec)),
                        )
                        .await
                        .unwrap();
                }
            });
            handle.await.unwrap();
        }
        "connect" => {
            let signing_key = SigningKey::new(thread_rng());
            let verification_key = signing_key.verification_key();
            let public_key = verification_key.to_bytes();

            let (upload_sender, upload_receiver) = channel(100);
            let (download_sender, download_receiver) = channel(100);
            let router = Router::new(public_key, download_sender, upload_receiver);
            let handle = router.start().await;
            info!(
                "Router {}",
                serde_json::to_string(&VerificationKeyBytes::from(verification_key)).unwrap()
            );

            let connect_addr = args().nth(2).unwrap();
            let connect_key = args().nth(3).unwrap();
            let connect_key: VerificationKeyBytes =
                serde_json::from_str(connect_key.as_str()).unwrap();
            info!("Connecting to {}", connect_addr);
            let socket = TcpStream::connect(connect_addr).await.unwrap();
            let (reader, writer) = socket.into_split();

            router.add_peer(
                    connect_key.to_bytes(),
                    UploadConnection::Tcp(FramedWrite::new(writer, PineconeCodec)),
                    DownloadConnection::Tcp(FramedRead::new(reader, PineconeCodec)),
                )
                .await;
            handle.await.unwrap();
        }
        _ => {}
    }
}
