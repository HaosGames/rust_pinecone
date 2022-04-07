use crate::frames::{Frame, TreeAnnouncement};
use crate::router::{Event, Router, VerificationKey};
use crate::tree::{Root, RootAnnouncementSignature};
use crate::wire_frame::PineconeCodec;
use env_logger::WriteStyle;
use log::{debug, info, trace, LevelFilter};
use std::env::args;
use std::time::SystemTime;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc::{channel, Receiver, Sender};
use tokio_util::codec::Framed;

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
            info!("New Client: {:?}", addr);

            let (upload_sender, upload_receiver) = channel(100);
            let (download_sender, download_receiver) = channel(100);
            let router = Router::new(public_key0, download_sender, upload_receiver);
            let handle = router.spawn();
            upload_sender
                .send(Event::AddPeer {
                    key: public_key1,
                    socket: Framed::new(socket, PineconeCodec),
                })
                .await
                .unwrap();
            let announcement = Frame::TreeAnnouncement(TreeAnnouncement {
                root: Root {
                    public_key: public_key0,
                    sequence_number: 0,
                },
                signatures: vec![RootAnnouncementSignature {
                    signing_public_key: public_key0,
                    destination_port: 1,
                }],
                receive_time: SystemTime::now(),
                receive_order: 0,
            });
            upload_sender
                .send(Event::SendFrameToPeer {
                    frame: announcement,
                    to: public_key1,
                })
                .await
                .unwrap();
            let router = handle.await.unwrap();
        }
        "1" => {
            info!("Connecting to 127.0.0.1:8080");
            let socket = TcpStream::connect("127.0.0.1:8080").await.unwrap();
            info!("Connected to 127.0.0.1:8080");

            let (upload_sender, upload_receiver) = channel(100);
            let (download_sender, download_receiver) = channel(100);
            let router = Router::new(public_key1, download_sender, upload_receiver);
            let handle = router.spawn();
            upload_sender
                .send(Event::AddPeer {
                    key: public_key0,
                    socket: Framed::new(socket, PineconeCodec),
                })
                .await
                .unwrap();
            let announcement = Frame::TreeAnnouncement(TreeAnnouncement {
                root: Root {
                    public_key: public_key1,
                    sequence_number: 0,
                },
                signatures: vec![RootAnnouncementSignature {
                    signing_public_key: public_key1,
                    destination_port: 1,
                }],
                receive_time: SystemTime::now(),
                receive_order: 0,
            });
            upload_sender
                .send(Event::SendFrameToPeer {
                    frame: announcement,
                    to: public_key0,
                })
                .await
                .unwrap();
            let router = handle.await.unwrap();
        }
        _ => {}
    }
}
