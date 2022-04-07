use crate::coordinates::Coordinates;
use crate::frames::{SnekBootstrapAck, SnekSetup, TreePacket};
use crate::router::{Port, SequenceNumber, VerificationKey};
use std::cmp::Ordering;
use std::fmt::{Display, Formatter};

#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct Root {
    pub(crate) public_key: VerificationKey,
    pub(crate) sequence_number: SequenceNumber,
}
impl PartialOrd<Self> for Root {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        let key = self.public_key.cmp(&other.public_key);
        if key != Ordering::Equal {
            return Some(key);
        } else {
            let sequence = self.sequence_number.cmp(&other.sequence_number);
            Some(sequence)
        }
    }
}
impl Ord for Root {
    fn cmp(&self, other: &Self) -> Ordering {
        self.partial_cmp(other)
            .expect("Comparing Roots should always lead to a result")
    }
}
impl Display for Root {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "[sequence:{},addr:{:?}]",
            self.sequence_number, self.public_key
        )
    }
}
#[derive(Clone, PartialEq, Debug)]
pub(crate) struct RootAnnouncementSignature {
    pub(crate) signing_public_key: VerificationKey,
    pub(crate) destination_port: Port,
    //TODO
    // pub(crate) signature: Signature,
}
pub(crate) trait TreeRouted {
    fn destination_coordinates(&self) -> Coordinates;
}
impl TreeRouted for TreePacket {
    fn destination_coordinates(&self) -> Coordinates {
        self.destination_coordinates.clone()
    }
}
impl TreeRouted for SnekBootstrapAck {
    fn destination_coordinates(&self) -> Coordinates {
        self.destination_coordinates.clone()
    }
}
impl TreeRouted for SnekSetup {
    fn destination_coordinates(&self) -> Coordinates {
        self.destination.clone()
    }
}
