use crate::frames::{Frame, TreeAnnouncement};
use crate::router::{Router, VerificationKey};
use crate::tree::{Root, RootAnnouncementSignature};
use crate::wire_frame::PineconeCodec;
use env_logger::WriteStyle;
use log::{debug, info, trace, LevelFilter};
use std::env::args;
use std::time::SystemTime;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc::{channel, Receiver, Sender};
use tokio_util::codec::{Framed, FramedRead, FramedWrite};
use crate::connection::{DownloadConnection, UploadConnection};

mod coordinates;
mod frames;
mod router;
mod snek;
mod tree;
mod wait_timer;
mod wire_frame;
mod connection;

#[tokio::main]
async fn main() {
    let _ = env_logger::builder()
        .write_style(WriteStyle::Always)
        .format_timestamp(None)
        .filter_level(LevelFilter::Debug)
        .filter_module("rust_pinecone", LevelFilter::Trace)
        // .filter_module("armaged", LevelFilter::Trace)
        .init();
    let public_key0 = [0; 32];
    let public_key1 = [1; 32];
    let mode = args().nth(1).unwrap();
    match mode.as_str() {
        "0" => {
            let listener = TcpListener::bind("127.0.0.1:8080").await.unwrap();
            info!("Listening on 127.0.0.1:8080");
            let (socket, addr) = listener.accept().await.unwrap();
            let (reader, writer) = socket.into_split();
            info!("New Client: {:?}", addr);

            let (upload_sender, upload_receiver) = channel(100);
            let (download_sender, download_receiver) = channel(100);
            let router = Router::new(public_key0, download_sender, upload_receiver);
            let handle = router.start().await;
            router
                .add_peer(public_key1, UploadConnection::Tcp(FramedWrite::new(writer, PineconeCodec)),
                          DownloadConnection::Tcp(FramedRead::new(reader, PineconeCodec)))
                .await;
            handle.await;
        }
        "1" => {
            info!("Connecting to 127.0.0.1:8080");
            let socket = TcpStream::connect("127.0.0.1:8080").await.unwrap();
            let (reader, writer) = socket.into_split();
            info!("Connected to 127.0.0.1:8080");

            let (upload_sender, upload_receiver) = channel(100);
            let (download_sender, download_receiver) = channel(100);
            let router = Router::new(public_key1, download_sender, upload_receiver);
            let handle = router.start().await;
            router
                .add_peer(public_key0, UploadConnection::Tcp(FramedWrite::new(writer, PineconeCodec)),
                          DownloadConnection::Tcp(FramedRead::new(reader, PineconeCodec)))
                .await;
            handle.await;
        }
        _ => {}
    }
}
