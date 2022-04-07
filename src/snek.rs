use crate::frames::{SnekBootstrap, SnekPacket, SnekSetup};
use crate::router::{Port, SnekPathId, VerificationKey, SNEK_EXPIRY_PERIOD};
use crate::tree::Root;
use std::time::SystemTime;

#[derive(PartialEq, Eq, Clone, Debug, PartialOrd, Ord, Hash)]
pub(crate) struct SnekPathIndex {
    pub(crate) public_key: VerificationKey,
    pub(crate) path_id: SnekPathId,
}
#[derive(PartialEq, Eq, Clone, Debug)]
pub(crate) struct SnekPath {
    pub(crate) origin: VerificationKey,
    pub(crate) target: VerificationKey,
    pub(crate) source: Port,
    pub(crate) destination: Port,
    pub(crate) last_seen: SystemTime,
    pub(crate) root: Root,
    pub(crate) active: bool,
}
impl SnekPath {
    /// `valid` returns true if the update hasn't expired, or false if it has. It is
    /// required for updates to time out eventually, in the case that paths don't get
    /// torn down properly for some reason.
    pub(crate) fn valid(&self) -> bool {
        self.last_seen.elapsed().unwrap() < SNEK_EXPIRY_PERIOD
    }
}
pub(crate) trait SnekRouted {
    fn destination_key(&self) -> VerificationKey;
}
impl SnekRouted for SnekPacket {
    fn destination_key(&self) -> VerificationKey {
        self.destination_key
    }
}
impl SnekRouted for SnekSetup {
    fn destination_key(&self) -> VerificationKey {
        self.destination_key
    }
}
impl SnekRouted for SnekBootstrap {
    fn destination_key(&self) -> VerificationKey {
        self.destination_key
    }
}
