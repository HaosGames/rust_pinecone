//! A rust implementation of the pinecone peer-to-peer overlay routing mechanism originally
//! implemented in [Pinecone](https://github.com/matrix-org/pinecone).

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


pub use crate::client::Client;
pub use crate::connection::{DownloadConnection, UploadConnection};
pub use crate::session::Session;

#[cfg(test)]
#[allow(unused)]
mod test {}