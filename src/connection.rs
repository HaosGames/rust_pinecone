use crate::error::RouterError;
use crate::{Frame, PineconeCodec};
use futures::SinkExt;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::mpsc::{channel, Receiver, Sender};
use tokio_stream::StreamExt;
use tokio_util::codec::{FramedRead, FramedWrite};

pub enum DownloadConnection {
    Tcp(FramedRead<OwnedReadHalf, PineconeCodec>),
    #[cfg(test)]
    Test(Receiver<Frame>),
}
pub enum UploadConnection {
    Tcp(FramedWrite<OwnedWriteHalf, PineconeCodec>),
    #[cfg(test)]
    Test(Sender<Frame>),
}
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
impl UploadConnection {
    pub(crate) async fn send(&mut self, frame: Frame) -> Result<(), RouterError> {
        match self {
            UploadConnection::Tcp(sink) => Ok(sink.send(frame).await?),
            #[cfg(test)]
            UploadConnection::Test(sink) => Ok(sink.send(frame).await?),
        }
    }
}
#[cfg(test)]
pub(crate) fn new_test_connection() -> (
    UploadConnection,
    DownloadConnection,
    UploadConnection,
    DownloadConnection,
) {
    let (peer0_sender, peer1_receiver) = channel(10);
    let (peer1_sender, peer0_receiver) = channel(10);
    (
        UploadConnection::Test(peer0_sender),
        DownloadConnection::Test(peer0_receiver),
        UploadConnection::Test(peer1_sender),
        DownloadConnection::Test(peer1_receiver),
    )
}
