mod protobuf {
    include!(concat!(env!("OUT_DIR"), "/minecraft_relay_protocol.rs"));
}

use bytes::{Buf, BufMut};
use prost::Message;

pub use protobuf::*;

impl RelayMessage {
    pub fn decode<B: Buf>(data: B) -> Result<Self, prost::DecodeError> {
        Message::decode(data)
    }

    pub fn encoded_len(&self) -> usize {
        Message::encoded_len(self)
    }

    pub fn encode<B: BufMut>(&self, buf: &mut B) -> Result<(), prost::EncodeError> {
        Message::encode(self, buf)
    }
}
