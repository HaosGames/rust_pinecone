use crate::coordinates::Coordinates;
use crate::router::{Port, PublicKey, SequenceNumber, SnekPathId};
use crate::tree::{Root, RootAnnouncementSignature};
use bytes::{BufMut, Bytes, BytesMut};
use ed25519_consensus::{Signature, SigningKey, VerificationKey, VerificationKeyBytes};
use log::trace;
use std::fmt::{Display, Formatter};
use std::time::SystemTime;

#[derive(Debug, Clone)]
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
    pub destination_key: PublicKey,
    pub source_key: PublicKey,
    pub payload: Vec<u8>,
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
    pub(crate) fn append_signature(&mut self, keypair: SigningKey, destination_port: Port) {
        let mut unsigned = BytesMut::new();
        let public_key = keypair.verification_key().to_bytes();
        unsigned.put_slice(&self.root.public_key);
        unsigned.put_u64(self.root.sequence_number);
        for sig in &self.signatures {
            unsigned.put_slice(&sig.signing_public_key);
            unsigned.put_u64(sig.destination_port);
            unsigned.put_slice(&sig.signature.to_bytes());
        }
        unsigned.put_slice(&public_key);
        unsigned.put_u64(destination_port);
        let signature = keypair.sign(unsigned.as_ref());
        self.signatures.push(RootAnnouncementSignature {
            signing_public_key: public_key,
            destination_port,
            signature,
        });
    }
    pub(crate) fn is_clean(&self, peer_public_key: &PublicKey) -> bool {
        if self.root.public_key != self.signatures.get(0).unwrap().signing_public_key {
            trace!("root_public_key doesn't match the first signing_public_key");
            return false;
        }
        if peer_public_key
            != &self
                .signatures
                .get(self.signatures.len() - 1)
                .unwrap()
                .signing_public_key
        {
            trace!("the last signing key doesn't match the key the announcement came from");
            return false;
        }
        for signature in &self.signatures {
            if signature.destination_port == 0 {
                trace!("destination port is 0");
                return false;
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
                    trace!("loop detected");
                    return false;
                }
            }
        }
        let mut to_verify = BytesMut::new();
        to_verify.put_slice(&self.root.public_key);
        to_verify.put_u64(self.root.sequence_number);
        for sig in &self.signatures {
            to_verify.put_slice(&sig.signing_public_key);
            to_verify.put_u64(sig.destination_port);
            if let Ok(key) = VerificationKey::try_from(sig.signing_public_key) {
                match key.verify(&sig.signature, to_verify.as_ref()) {
                    Ok(_) => {
                        to_verify.put_slice(sig.signature.to_bytes().as_ref());
                    }
                    Err(e) => {
                        trace!("signature verification failed: {:?}", e);
                        return false;
                    }
                }
            } else {
                trace!("signing_public_key parsing failed");
                return false;
            }
        }
        true
    }
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
