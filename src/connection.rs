use crate::error::RouterError;
use crate::frames::Frame;
use crate::wire_frame::PineconeCodec;
use futures::SinkExt;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
#[cfg(test)]
use tokio::net::{TcpListener, TcpStream};
#[cfg(test)]
use tokio::sync::mpsc::{Receiver, Sender};
use tokio_stream::StreamExt;
use tokio_util::codec::{FramedRead, FramedWrite};

#[derive(Debug)]
pub enum DownloadConnection {
    Tcp(FramedRead<OwnedReadHalf, PineconeCodec>),
    #[cfg(test)]
    #[allow(unused)]
    Test(Receiver<Frame>),
}
#[derive(Debug)]
pub enum UploadConnection {
    Tcp(FramedWrite<OwnedWriteHalf, PineconeCodec>),
    #[cfg(test)]
    #[allow(unused)]
    Test(Sender<Frame>),
}
#[allow(unused)]
impl DownloadConnection {
    pub(crate) async fn next(&mut self) -> Option<Result<Frame, RouterError>> {
        return match self {
            DownloadConnection::Tcp(stream) => stream.next().await,
            #[cfg(test)]
            DownloadConnection::Test(stream) => match stream.recv().await {
                None => None,
                Some(frame) => Some(Ok(frame)),
            },
        };
    }
}
#[allow(unused)]
impl UploadConnection {
    pub(crate) async fn send(&mut self, frame: Frame) -> Result<(), RouterError> {
        match self {
            UploadConnection::Tcp(sink) => Ok(sink.send(frame).await?),
            #[cfg(test)]
            UploadConnection::Test(sink) => Ok(sink.send(frame).await?),
        }
    }
}
impl From<OwnedReadHalf> for DownloadConnection {
    fn from(stream: OwnedReadHalf) -> Self {
        Self::Tcp(FramedRead::new(stream, PineconeCodec))
    }
}
impl From<OwnedWriteHalf> for UploadConnection {
    fn from(sink: OwnedWriteHalf) -> Self {
        Self::Tcp(FramedWrite::new(sink, PineconeCodec))
    }
}
#[cfg(test)]
pub(crate) async fn new_test_connection() -> (
    Box<FramedWrite<OwnedWriteHalf, PineconeCodec>>,
    Box<FramedRead<OwnedReadHalf, PineconeCodec>>,
    Box<FramedWrite<OwnedWriteHalf, PineconeCodec>>,
    Box<FramedRead<OwnedReadHalf, PineconeCodec>>,
) {
    let bind = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = bind.local_addr().unwrap();
    let socket1 = tokio::spawn(async move {
        let (socket, _addr) = bind.accept().await.unwrap();
        socket
    });
    let socket2 = TcpStream::connect(addr).await.unwrap();
    let socket1 = socket1.await.unwrap();
    let (s1d, s1u) = socket1.into_split();
    let (s2d, s2u) = socket2.into_split();
    (
        Box::new(FramedWrite::new(s1u, PineconeCodec)),
        Box::new(FramedRead::new(s1d, PineconeCodec)),
        Box::new(FramedWrite::new(s2u, PineconeCodec)),
        Box::new(FramedRead::new(s2d, PineconeCodec)),
    )
}
