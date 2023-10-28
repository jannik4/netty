#![allow(clippy::type_complexity)]

// TODO: bound crossbeam channels capacity

mod channel;
mod channel_id;
mod client;
mod connection;
mod double_key_map;
mod handle;
mod protocol;
mod server;

pub mod transport;

use thiserror::Error;

pub use {
    self::{
        client::{Client, ClientEvent},
        handle::ConnectionHandle,
        server::{Server, ServerEvent},
    },
    bytes::Bytes,
    channel::Channels,
    channel_id::ChannelId,
};

pub type Result<T, E = NetworkError> = std::result::Result<T, E>;

// TODO: Clean up variants. Split into multiple errors?
#[derive(Debug, Error)]
pub enum NetworkError {
    #[error("encode")]
    Encode(#[from] EncodeError),
    #[error("decode")]
    Decode(#[from] DecodeError),
    #[error("not connected")]
    NotConnected,

    #[error("transport send")]
    TransportSend(#[source] Box<dyn std::error::Error + Send + Sync>),

    #[error("transport receive")]
    TransportReceive(#[source] Box<dyn std::error::Error + Send + Sync>),

    #[error("io")]
    Io(#[from] std::io::Error),

    #[error("other")]
    Other, // TODO: REMOVE ME !!!
}

#[derive(Debug, Error)]
#[error("encode")]
pub struct EncodeError(#[from] Box<dyn std::error::Error + Send + Sync>);

#[derive(Debug, Error)]
#[error("decode")]
pub struct DecodeError(#[from] Box<dyn std::error::Error + Send + Sync>);

pub trait NetworkEncode {
    fn encode(&self) -> Result<Bytes, EncodeError>;
}

#[cfg(feature = "serde")]
impl<T> NetworkEncode for T
where
    T: serde::Serialize,
{
    fn encode(&self) -> Result<Bytes, EncodeError> {
        use bincode::Options;

        match bincode::DefaultOptions::new().serialize(self) {
            Ok(buf) => Ok(Bytes::from(buf)),
            Err(e) => Err(EncodeError(Box::new(e))),
        }
    }
}

pub trait NetworkDecode: Sized {
    fn decode(buf: &[u8]) -> Result<Self, DecodeError>;
}

#[cfg(feature = "serde")]
impl<T> NetworkDecode for T
where
    T: serde::de::DeserializeOwned,
{
    fn decode(buf: &[u8]) -> Result<Self, DecodeError> {
        use bincode::Options;

        bincode::DefaultOptions::new().deserialize(buf).map_err(|e| DecodeError(Box::new(e)))
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ChannelConfig {
    pub reliable: bool,
    pub ordered: bool,
}

pub trait NetworkMessage {
    const CHANNEL_ID: ChannelId;
    const CHANNEL_CONFIG: ChannelConfig;
}

#[derive(Debug, Clone)]
struct Channel {
    id: ChannelId,
    config: ChannelConfig,
    ty: std::any::TypeId,
    ty_name: &'static str,
}
