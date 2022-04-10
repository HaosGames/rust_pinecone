use std::io::Error;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::mpsc::{Receiver, Sender};
use tokio_util::codec::{FramedRead, FramedWrite};
use crate::{Frame, PineconeCodec};
use tokio_stream::StreamExt;
use futures::SinkExt;

pub enum DownloadConnection {
    Tcp(FramedRead<OwnedReadHalf, PineconeCodec>),
    Test(Receiver<Frame>)
}
pub enum UploadConnection {
    Tcp(FramedWrite<OwnedWriteHalf, PineconeCodec>),
    Test(Sender<Frame>)
}
impl DownloadConnection {
    pub(crate) async fn next(&mut self) -> Option<Result<Frame, std::io::Error>> {
        return match self {
            DownloadConnection::Tcp(stream) => {
                stream.next().await
            }
            DownloadConnection::Test(stream) => {
                match stream.recv().await {
                    None => { None }
                    Some(frame) => {
                        Some(Ok(frame))
                    }
                }
            }
        }
    }
}
impl UploadConnection {
    pub(crate) async fn send(&mut self, frame: Frame) {
        match self {
            UploadConnection::Tcp(sink) => {
                sink.send(frame).await.unwrap();
            }
            UploadConnection::Test(sink) => {
                sink.send(frame).await.unwrap();
            }
        }
    }
}
