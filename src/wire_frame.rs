use crate::coordinates::Coordinates;
use crate::error::RouterError;
use crate::frames::{
    Frame, SnekBootstrap, SnekBootstrapAck, SnekPacket, SnekSetup, SnekSetupAck, SnekTeardown,
    TreeAnnouncement, TreePacket,
};
use crate::router::PublicKey;
use crate::tree::{Root, RootAnnouncementSignature};
use bytes::{Buf, BufMut, BytesMut};
use log::{debug, trace};
use std::io::{ErrorKind, Read};
use std::time::SystemTime;
use tokio_util::codec::{Decoder, Encoder};

/// MaxFrameSize is the maximum size that a single frame can be, including
/// all headers.
const MAX_FRAME_SIZE: u32 = 65535 * 3 + 16;
/// MaxPayloadSize is the maximum size that a single frame can contain
/// as a payload, not including headers.
const MAX_PAYLOAD_SIZE: u16 = 65535;
const FRAME_MAGIC_BYTES: [u8; 4] = [0x70, 0x69, 0x6e, 0x65];
/// 4 magic bytes, 1 byte version, 1 byte type, 2 bytes extra, 2 bytes frame length
const FRAME_HEADER_LENGTH: u32 = 10;

pub struct WireFrame {
    // Header
    version: u8,
    frame_type: u8,
    extra: [u8; 2],
    frame_length: u16,

    // Payload
    destination_len: u16,
    destination: Option<Coordinates>,
    destination_key: Option<PublicKey>,
    source_len: u16,
    source: Option<Coordinates>,
    source_key: Option<PublicKey>,
    payload: Vec<u8>,
}
#[derive(Debug)]
pub struct PineconeCodec;
impl Encoder<Frame> for PineconeCodec {
    type Error = RouterError;

    fn encode(&mut self, item: Frame, dst: &mut BytesMut) -> Result<(), Self::Error> {
        if !dst.is_empty() {
            return Err(Self::Error::EncodingError(
                "Buffer to write to is not empty",
            ));
        }
        let len = match &item {
            Frame::TreeRouted(packet) => {
                10 + 2
                    + packet.destination_coordinates.coordinates.len() * 8
                    + 2
                    + packet.source_coordinates.coordinates.len() * 8
                    + packet.payload.len()
            }
            Frame::SnekRouted(packet) => 10 + 32 + 32 + packet.payload.len(),
            Frame::TreeAnnouncement(packet) => 10 + 32 + 8 + 2 + packet.signatures.len() * (32 + 8),
            Frame::SnekBootstrap(packet) => {
                10 + 32 + 2 + packet.source.coordinates.len() * 8 + 32 + 8 + 8
            }
            Frame::SnekBootstrapACK(packet) => {
                10 + 2
                    + packet.destination_coordinates.coordinates.len() * 8
                    + 32
                    + 2
                    + packet.source_coordinates.coordinates.len() * 8
                    + 32
                    + 32
                    + 8
                    + 8
            }
            Frame::SnekSetup(packet) => {
                10 + 2 + packet.destination.coordinates.len() * 8 + 32 + 32 + 32 + 8 + 8
            }
            Frame::SnekSetupACK(packet) => 10 + 32 + 32 + 8 + 8,
            Frame::SnekTeardown(packet) => 10 + 32 + 32 + 8 + 8,
        } as u16;
        dst.reserve(len as usize);

        dst.put_slice(FRAME_MAGIC_BYTES.as_slice());
        dst.put_u8(0); // FrameVersion
        dst.put_u8(match &item {
            // FrameType
            Frame::TreeRouted(_) => 2,
            Frame::SnekRouted(_) => 8,
            Frame::TreeAnnouncement(_) => 1,
            Frame::SnekBootstrap(_) => 3,
            Frame::SnekBootstrapACK(_) => 4,
            Frame::SnekSetup(_) => 5,
            Frame::SnekSetupACK(_) => 6,
            Frame::SnekTeardown(_) => 7,
        });
        dst.put_u16(0);
        dst.put_u16(len);
        match item {
            Frame::TreeRouted(packet) => {
                dst.put_u16(packet.destination_coordinates.coordinates.len() as u16);
                for coord in packet.destination_coordinates.coordinates {
                    dst.put_u64(coord);
                }
                dst.put_u16(packet.source_coordinates.coordinates.len() as u16);
                for coord in packet.source_coordinates.coordinates {
                    dst.put_u64(coord);
                }
                dst.put_slice(packet.payload.as_slice());
            }
            Frame::SnekRouted(packet) => {
                dst.put_slice(packet.destination_key.as_slice());
                dst.put_slice(packet.source_key.as_slice());
                dst.put_slice(packet.payload.as_slice());
            }
            Frame::TreeAnnouncement(packet) => {
                dst.put_slice(packet.root.public_key.as_slice());
                dst.put_u64(packet.root.sequence_number);
                dst.put_u16(packet.signatures.len() as u16);
                for sig in packet.signatures {
                    dst.put_slice(sig.signing_public_key.as_slice());
                    dst.put_u64(sig.destination_port);
                }
            }
            Frame::SnekBootstrap(packet) => {
                dst.put_slice(packet.destination_key.as_slice());
                dst.put_u16(packet.source.coordinates.len() as u16);
                for port in packet.source.coordinates {
                    dst.put_u64(port);
                }
                dst.put_slice(packet.root.public_key.as_slice());
                dst.put_u64(packet.root.sequence_number);
                dst.put_u64(packet.path_id);
            }
            Frame::SnekBootstrapACK(packet) => {
                dst.put_u16(packet.destination_coordinates.coordinates.len() as u16);
                for coord in packet.destination_coordinates.coordinates {
                    dst.put_u64(coord);
                }
                dst.put_slice(packet.destination_key.as_slice());
                dst.put_u16(packet.source_coordinates.coordinates.len() as u16);
                for coord in packet.source_coordinates.coordinates {
                    dst.put_u64(coord);
                }
                dst.put_slice(packet.source_key.as_slice());
                dst.put_slice(packet.root.public_key.as_slice());
                dst.put_u64(packet.root.sequence_number);
                dst.put_u64(packet.path_id);
            }
            Frame::SnekSetup(packet) => {
                dst.put_u16(packet.destination.coordinates.len() as u16);
                for coord in packet.destination.coordinates {
                    dst.put_u64(coord);
                }
                dst.put_slice(packet.destination_key.as_slice());
                dst.put_slice(packet.source_key.as_slice());
                dst.put_slice(packet.root.public_key.as_slice());
                dst.put_u64(packet.root.sequence_number);
                dst.put_u64(packet.path_id);
            }
            Frame::SnekSetupACK(packet) => {
                dst.put_slice(packet.destination_key.as_slice());
                dst.put_slice(packet.root.public_key.as_slice());
                dst.put_u64(packet.root.sequence_number);
                dst.put_u64(packet.path_id);
            }
            Frame::SnekTeardown(packet) => {
                dst.put_slice(packet.destination_key.as_slice());
                dst.put_slice(packet.root.public_key.as_slice());
                dst.put_u64(packet.root.sequence_number);
                dst.put_u64(packet.path_id);
            }
        }
        Ok(())
    }
}
fn decode_key(src: &mut BytesMut) -> PublicKey {
    let mut key: PublicKey = [0; 32];
    for i in 0..32 {
        key[i] = src.get_u8();
    }
    key
}
impl Decoder for PineconeCodec {
    type Item = Frame;
    type Error = RouterError;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        if src.is_empty() {
            return Ok(None);
        }
        if src.len() < 4 {
            return Ok(None);
        }
        if !src.starts_with(FRAME_MAGIC_BYTES.as_slice()) {
            return Err(Self::Error::DecodingError("Magic Bytes not found"));
        }
        if src.len() < 10 {
            return Ok(None);
        }
        src.get_u32(); // Discard Magic Bytes
        if src.get_u8() != 0 {
            return Err(Self::Error::DecodingError("Not frame version 0"));
        }
        let frame_type = src.get_u8();
        let extra1 = src.get_u8();
        let extra2 = src.get_u8();
        let len = src.get_u16();
        if src.len() < (len - 10) as usize {
            return Err(Self::Error::DecodingError("Did not receive enough bytes"));
        }
        match frame_type {
            1  /*TreeAnnouncement*/ => {
                let root_key = decode_key(src);
                let root_sequence = src.get_u64();
                let sig_len = src.get_u16();
                let mut sigs = vec![];
                for i in 0..sig_len {
                    let sig_key = decode_key(src);
                    let sig_port = src.get_u64();
                    sigs.push(RootAnnouncementSignature {
                        signing_public_key: sig_key,
                        destination_port: sig_port
                    })
                }
                return Ok(Some(Frame::TreeAnnouncement(TreeAnnouncement {
                    root: Root {
                        public_key: root_key,
                        sequence_number: root_sequence
                    },
                    signatures: sigs,
                    receive_time: SystemTime::now(),
                    receive_order: 0
                })));
            }
            2 /*TreePacket*/ => {
                let dest_len = src.get_u16();
                let mut dest = vec![];
                for i in 0..dest_len {
                    dest.push(src.get_u64());
                }
                let source_len = src.get_u16();
                let mut source = vec![];
                for i in 0..dest_len {
                    source.push(src.get_u64());
                }
                let payload_len = len - 10 -4-dest_len*8-source_len*8;
                let mut payload = vec![];
                for i in 0..payload_len {
                    payload.push(src.get_u8());
                }
                return Ok(Some(Frame::TreeRouted(TreePacket {
                    source_coordinates: Coordinates::new(source),
                    destination_coordinates: Coordinates::new(dest),
                    payload
                })))
            }
            3 /*SnekBootstrap*/ => {
                let dest_key = decode_key(src);
                let source_len = src.get_u16();
                let mut source = vec![];
                for i in 0..source_len {
                    source.push(src.get_u64());
                }
                let root_key = decode_key(src);
                let root_sequence = src.get_u64();
                let path_id = src.get_u64();
                return Ok(Some(Frame::SnekBootstrap(SnekBootstrap {
                    root: Root { public_key: root_key, sequence_number: root_sequence },
                    destination_key: dest_key,
                    source: Coordinates::new(source),
                    path_id
                })));
            }
            4 /*SnekBootstrapAck*/ => {
                let dest_len = src.get_u16();
                let mut dest = vec![];
                for i in 0..dest_len {
                    dest.push(src.get_u64());
                }
                let dest_key = decode_key(src);
                let source_len = src.get_u16();
                let mut source = vec![];
                for i in 0..source_len {
                    source.push(src.get_u64());
                }
                let source_key = decode_key(src);
                let root_key = decode_key(src);
                let root_sequence = src.get_u64();
                let path_id = src.get_u64();
                return Ok(Some(Frame::SnekBootstrapACK(SnekBootstrapAck {
                    destination_coordinates: Coordinates::new(dest),
                    destination_key: dest_key,
                    source_coordinates: Coordinates::new(source),
                    source_key,
                    root: Root { public_key: root_key, sequence_number: root_sequence },
                    path_id
                })));
            }
            5 /*SnekSetup*/ => {
                let dest_len = src.get_u16();
                let mut dest = vec![];
                for i in 0..dest_len {
                    dest.push(src.get_u64());
                }
                let dest_key = decode_key(src);
                let source_key = decode_key(src);
                let root_key = decode_key(src);
                let root_sequence = src.get_u64();
                let path_id = src.get_u64();
                return Ok(Some(Frame::SnekSetup(SnekSetup {
                    root: Root { public_key: root_key, sequence_number: root_sequence },
                    destination: Coordinates::new(dest),
                    destination_key: dest_key,
                    source_key,
                    path_id
                })));
            }
            6 /*SnekSetupAck*/ => {
                let dest_key = decode_key(src);
                let root_key = decode_key(src);
                let root_sequence = src.get_u64();
                let path_id = src.get_u64();
                return Ok(Some(Frame::SnekSetupACK(SnekSetupAck {
                    root: Root { public_key: root_key, sequence_number: root_sequence },
                    destination_key: dest_key,
                    path_id
                })));
            }
            7 /*SnekTeardown*/ => {
                let dest_key = decode_key(src);
                let root_key = decode_key(src);
                let root_sequence = src.get_u64();
                let path_id = src.get_u64();
                return Ok(Some(Frame::SnekTeardown(SnekTeardown {
                    root: Root { public_key: root_key, sequence_number: root_sequence },
                    destination_key: dest_key,
                    path_id
                })));
            }
            8 /*SnekPacket*/ => {
                let dest_key = decode_key(src);
                let source_key = decode_key(src);
                let payload_len = len -10-64;
                let mut payload = vec![];
                for i in 0..payload_len {
                    payload.push(src.get_u8());
                }
                return Ok(Some(Frame::SnekRouted(SnekPacket {
                    destination_key: dest_key,
                    source_key,
                    payload
                })));
            }
            _ => {
                return Err(Self::Error::DecodingError("Not a supported frame type"));
            }
        }
    }
}
