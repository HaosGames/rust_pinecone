use crate::coordinates::Coordinates;
use crate::router::{Port, PublicKey, SequenceNumber, SnekPathId};
use crate::tree::{Root, RootAnnouncementSignature};
use std::fmt::{Display, Formatter};
use std::time::SystemTime;

#[derive(Debug)]
pub enum Frame {
    TreeRouted(TreePacket),
    SnekRouted(SnekPacket),
    TreeAnnouncement(TreeAnnouncement),
    SnekBootstrap(SnekBootstrap),
    SnekBootstrapACK(SnekBootstrapAck),
    SnekSetup(SnekSetup),
    SnekSetupACK(SnekSetupAck),
    SnekTeardown(SnekTeardown),
}
#[derive(Debug, Clone, PartialEq)]
pub struct SnekPacket {
    pub(crate) destination_key: PublicKey,
    pub(crate) source_key: PublicKey,
    pub(crate) payload: Vec<u8>,
}
#[derive(Debug, Clone, PartialEq)]
pub struct TreePacket {
    pub(crate) source_coordinates: Coordinates,
    pub(crate) destination_coordinates: Coordinates,
    pub(crate) payload: Vec<u8>,
}
#[derive(Debug, Clone, PartialEq)]
pub struct TreeAnnouncement {
    pub(crate) root: Root,
    pub(crate) signatures: Vec<RootAnnouncementSignature>,
    pub(crate) receive_time: SystemTime,
    pub(crate) receive_order: SequenceNumber,
}
impl TreeAnnouncement {
    pub(crate) fn append_signature(
        &self,
        public_key: PublicKey,
        destination_port: Port,
    ) -> TreeAnnouncement {
        // let public_key = keypair.public();
        let mut to_sign = RootAnnouncementSignature {
            signing_public_key: public_key,
            destination_port: destination_port.clone(),
            //TODO
            // signature: None,
        };
        //TODO
        // let unsigned = serde_json::to_vec(&to_sign).unwrap();
        // let serialized_unsigned = unsigned.as_slice();
        // let signature: Signature = keypair.sign(serialized_unsigned);
        // to_sign.signature = Some(signature);
        let mut announcement = self.clone();
        announcement.signatures.push(to_sign);
        announcement
    }
    /*pub(crate) fn is_clean(&self, peer_public_key: &VerificationKeyBytes) -> bool {
        if self.root.public_key != self.signatures.get(0).unwrap().signing_public_key {
            return false; // if root_public_key doesn't match the first signing_public_key
        }
        if peer_public_key
            != &self
            .signatures
            .get(self.signatures.len() - 1)
            .unwrap()
            .signing_public_key
        {
            return false; // if the last signing key doesn't match the key the announcement came from
        }
        for signature in &self.signatures {
            if signature.destination_port == 0 {
                return false; // If a destination port is 0
            }
        }
        for i in 0..self.signatures.len() - 1 {
            for j in 0..self.signatures.len() - 1 {
                if i == j {
                    continue;
                }
                if self.signatures.get(i).unwrap().signing_public_key
                    == self.signatures.get(j).unwrap().signing_public_key
                {
                    return false;
                }
            }
        }

        // TODO: Check signatures

        true
    }*/
    pub(crate) fn is_loop_of_child(&self, public_key: &PublicKey) -> bool {
        for signature in &self.signatures {
            if &signature.signing_public_key == public_key {
                return true;
            }
        }
        false
    }
    pub(crate) fn replayed_old_sequence(&self, of: &TreeAnnouncement) -> bool {
        if self.root.sequence_number < of.root.sequence_number {
            true
        } else {
            false
        }
    }
    pub(crate) fn replayed_current_sequence(&self, of: &TreeAnnouncement) -> bool {
        if self.root.sequence_number == of.root.sequence_number {
            true
        } else {
            false
        }
    }
    pub(crate) fn has_same_root_key(&self, as_: &TreeAnnouncement) -> bool {
        if self.root.public_key == as_.root.public_key {
            true
        } else {
            false
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SnekBootstrap {
    pub(crate) root: Root,
    pub(crate) destination_key: PublicKey,
    pub(crate) source: Coordinates,
    pub(crate) path_id: SnekPathId,
}
#[derive(Debug, Clone, PartialEq)]
pub struct SnekBootstrapAck {
    pub(crate) destination_coordinates: Coordinates,
    pub(crate) destination_key: PublicKey,
    pub(crate) source_coordinates: Coordinates,
    pub(crate) source_key: PublicKey,
    pub(crate) root: Root,
    pub(crate) path_id: SnekPathId,
}
#[derive(Debug, Clone, PartialEq)]
pub struct SnekSetup {
    pub(crate) root: Root,
    pub(crate) destination: Coordinates,
    pub(crate) destination_key: PublicKey,
    pub(crate) source_key: PublicKey,
    pub(crate) path_id: SnekPathId,
}
#[derive(Debug, Clone, PartialEq)]
pub struct SnekSetupAck {
    pub(crate) root: Root,
    pub(crate) destination_key: PublicKey,
    pub(crate) path_id: SnekPathId,
}
#[derive(Debug, Clone, PartialEq)]
pub struct SnekTeardown {
    pub(crate) root: Root,
    pub(crate) destination_key: PublicKey,
    pub(crate) path_id: SnekPathId,
}

impl Display for TreeAnnouncement {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let mut sigs = String::new();
        for sig in &self.signatures {
            sigs = format!("{},{}", sigs, sig);
        }
        write!(f, "[root:{},signatures:({})]", self.root, sigs)
    }
}
impl Display for RootAnnouncementSignature {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "[port:{},addr:]", self.destination_port)
    }
}
