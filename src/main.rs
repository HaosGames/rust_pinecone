use crate::client::Client;
use crate::wire_frame::PineconeCodec;
use ed25519_consensus::{SigningKey, VerificationKey, VerificationKeyBytes};
use env_logger::WriteStyle;
use log::{debug, info, warn, LevelFilter};
use rand::thread_rng;
use std::env::args;
use std::io::Write;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
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
        //.filter_module("rust_pinecone", LevelFilter::Trace)
        .init();

    let signing_key = SigningKey::new(thread_rng());
    let verification_key = signing_key.verification_key();
    let (client, mut session_listener) = Client::new(signing_key).await;
    info!(
        "Router {}",
        serde_json::to_string(&VerificationKeyBytes::from(verification_key)).unwrap()
    );

    let listen_addr = args()
        .nth(1)
        .or_else(|| Some(String::from("127.0.0.1:0")))
        .unwrap();
    let listener = TcpListener::bind(listen_addr).await.unwrap();
    info!("Listening on {}", listener.local_addr().unwrap());
    let client1 = client.clone();
    tokio::spawn(async move {
        loop {
            let (socket, addr) = listener.accept().await.unwrap();
            let (reader, writer) = socket.into_split();
            info!("New Client: {:?}", addr);
            client1
                .connect_peer(
                    Box::new(FramedWrite::new(writer, PineconeCodec)),
                    Box::new(FramedRead::new(reader, PineconeCodec)),
                )
                .await
                .unwrap();
        }
    });
    tokio::spawn(async move {
        loop {
            match session_listener.recv().await {
                Some(mut session) => {
                    debug!("New session with {:?}", session.peer_key());
                    tokio::spawn(async move {
                        loop {
                            let mut buf = [0u8; 20];
                            match session.read(&mut buf).await {
                                Ok(0) => {
                                    debug!("session with {:?} ended", session.peer_key());
                                    break;
                                }
                                Err(e) => {
                                    warn!("error while reading from session: {:?}", e);
                                    break;
                                }
                                _ => {}
                            }
                            let message = String::from_utf8(buf.to_vec())
                                .or_else::<(), _>(|_| Ok(String::from("Message was not UTF-8")))
                                .unwrap();
                            println!("{:?}> {}", session.peer_key(), message);
                        }
                    });
                }
                None => {
                    warn!("session_listener stream ended");
                    break;
                }
            }
        }
    });

    loop {
        println!("Available actions:");
        println!("1) Add peer");
        println!("2) Chat with peer");
        println!("3) Disconnect peer");
        println!("4) Stop router");
        match read_stdin_line().await.as_str() {
            "1" => {
                println!("Address of peer:");
                let connect_addr = read_stdin_line().await;
                info!("Connecting to {}", connect_addr);
                let socket = TcpStream::connect(connect_addr).await.unwrap();
                let (reader, writer) = socket.into_split();
                match client
                    .connect_peer(
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
                if let Ok(target_key) = read_public_key().await {
                    println!("Entered chat");
                    let mut send_session = client.dial_send(target_key.to_bytes()).await;
                    loop {
                        print!("> ");
                        std::io::stdout().flush().unwrap();
                        let input = read_stdin_line().await;
                        if input.as_str() == "exit" {
                            break;
                        }
                        send_session.write_all(input.as_bytes()).await.unwrap();
                    }
                } else {
                    println!("Invalid key");
                }
            }
            "3" => {
                println!("Public Key:");
                if let Ok(target_key) = read_public_key().await {
                    client.disconnect_peer(target_key.to_bytes()).await;
                } else {
                    println!("Invalid key");
                }
            }
            "4" => {
                client.stop().await;
                break;
            }
            _ => {}
        }
    }
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
async fn read_public_key() -> serde_json::Result<VerificationKey> {
    let input = read_stdin_line().await;
    serde_json::from_str(input.as_str())
}
