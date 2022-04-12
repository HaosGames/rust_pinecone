use crate::Frame;
use std::fmt::{Display, Formatter};

#[derive(Debug)]
pub enum RouterError {
    ConnectionClosed,
    MissingSignature,
    InvalidFrame,
    DecodingError(&'static str),
    EncodingError(&'static str),
}
impl Display for RouterError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}
impl From<std::io::Error> for RouterError {
    fn from(e: std::io::Error) -> Self {
        match e.kind() {
            _ => RouterError::ConnectionClosed,
        }
    }
}
impl From<tokio::sync::mpsc::error::SendError<Frame>> for RouterError {
    fn from(e: tokio::sync::mpsc::error::SendError<Frame>) -> Self {
        RouterError::ConnectionClosed
    }
}
