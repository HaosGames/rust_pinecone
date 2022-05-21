use crate::connection::{DownloadConnection, UploadConnection};
use crate::frames::{Frame, SnekPacket};
use crate::router::Router;
use crate::wire_frame::PineconeCodec;
use ed25519_consensus::{SigningKey, VerificationKeyBytes};
use env_logger::WriteStyle;
use log::{debug, info, trace, LevelFilter};
use rand::thread_rng;
use std::env::args;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc::channel;
use tokio_util::codec::{FramedRead, FramedWrite};

mod client;
mod connection;
mod coordinates;
mod error;
mod frames;
mod router;
mod session;
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

    let signing_key = SigningKey::new(thread_rng());
    let verification_key = signing_key.verification_key();
    let public_key = verification_key.to_bytes();
    let (upload_sender, upload_receiver) = channel(100);
    let (download_sender, mut download_receiver) = channel(100);
    let router = Router::new(public_key, download_sender, upload_receiver);
    let handle = router.start().await;
    info!(
        "Router {}",
        serde_json::to_string(&VerificationKeyBytes::from(verification_key)).unwrap()
    );

    let listen_addr = args().nth(1).or(Some(String::from("127.0.0.1:0"))).unwrap();
    let listener = TcpListener::bind(listen_addr).await.unwrap();
    info!("Listening on {}", listener.local_addr().unwrap());
    let router1 = router.clone();
    tokio::spawn(async move {
        loop {
            let (socket, addr) = listener.accept().await.unwrap();
            let (reader, writer) = socket.into_split();
            info!("New Client: {:?}", addr);
            router1
                .connect(
                    Box::new(FramedWrite::new(writer, PineconeCodec)),
                    Box::new(FramedRead::new(reader, PineconeCodec)),
                )
                .await
                .unwrap();
        }
    });
    tokio::spawn(async move {
        loop {
            match download_receiver.recv().await {
                Some(frame) => match frame {
                    Frame::SnekRouted(packet) => {
                        println!("Received message from {:?}", packet.source_key);
                        let message = String::from_utf8(packet.payload)
                            .or::<()>(Ok(String::from("Message was not UTF-8")))
                            .unwrap();
                        println!("Message: {}", message);
                    }
                    _ => {}
                },
                None => {
                    debug!("Local download channel broke.");
                    break;
                }
            }
        }
    });

    loop {
        println!("Available actions:");
        println!("1) Add peer");
        println!("2) Send message");
        println!("3) Disconnect peer");
        println!("4) Stop router");
        match read_stdin_line().await.as_str() {
            "1" => {
                println!("Address of peer:");
                let connect_addr = read_stdin_line().await;
                info!("Connecting to {}", connect_addr);
                let socket = TcpStream::connect(connect_addr).await.unwrap();
                let (reader, writer) = socket.into_split();
                match router
                    .connect(
                        Box::new(FramedWrite::new(writer, PineconeCodec)),
                        Box::new(FramedRead::new(reader, PineconeCodec)),
                    )
                    .await
                {
                    Ok(_) => {}
                    Err(e) => {
                        info!("Could not connect to peer: {:?}", e);
                    }
                }
            }
            "2" => {
                println!("Target key:");
                let input = read_stdin_line().await;
                let target_key: VerificationKeyBytes =
                    serde_json::from_str(input.as_str()).unwrap();
                println!("Message:");
                let message = read_stdin_line().await;
                let payload = message.as_bytes().to_vec();
                upload_sender
                    .send(Frame::SnekRouted(SnekPacket {
                        destination_key: target_key.to_bytes(),
                        source_key: public_key,
                        payload,
                    }))
                    .await
                    .unwrap();
            }
            "3" => {
                println!("Public Key:");
                let input = read_stdin_line().await;
                let target_key: VerificationKeyBytes =
                    serde_json::from_str(input.as_str()).unwrap();
                router.disconnect_peer(target_key.to_bytes()).await;
            }
            "4" => {
                router.stop().await;
                drop(upload_sender);
                break;
            }
            _ => {}
        }
    }
    handle.await.unwrap();
}
async fn read_stdin_line() -> String {
    tokio::task::spawn_blocking(|| {
        let mut result = String::new();
        std::io::stdin().read_line(&mut result).unwrap();
        String::from(result.strip_suffix("\n").unwrap())
    })
    .await
    .unwrap()
}
