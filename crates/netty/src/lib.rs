#![allow(clippy::type_complexity)]

// TODO: bound channels capacity

mod channel;
mod channel_id;
mod client;
mod connection;
mod handle;
mod new_data;
mod protocol;
mod server;

pub mod transport;

use std::{future::Future, time::Duration};
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

pub trait Runtime: native_only_tokio::NativeOnlyTokio + Send + Sync + 'static {
    fn spawn<F>(&self, f: F)
    where
        F: Future<Output = ()> + WasmNotSend + 'static;

    fn sleep(&self, duration: Duration) -> impl Future<Output = ()> + WasmNotSend;
}

mod native_only_tokio {
    pub trait NativeOnlyTokio {}

    #[cfg(target_arch = "wasm32")]
    impl<T> NativeOnlyTokio for T {}

    #[cfg(not(target_arch = "wasm32"))]
    impl NativeOnlyTokio for super::NativeRuntime {}

    #[cfg(not(target_arch = "wasm32"))]
    impl NativeOnlyTokio for &'static super::NativeRuntime {}
}

#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug)]
pub struct NativeRuntime(pub tokio::runtime::Runtime);

#[cfg(not(target_arch = "wasm32"))]
impl Runtime for NativeRuntime {
    fn spawn<F>(&self, f: F)
    where
        F: Future<Output = ()> + WasmNotSend + 'static,
    {
        self.0.spawn(f);
    }

    fn sleep(&self, duration: Duration) -> impl Future<Output = ()> + WasmNotSend {
        tokio::time::sleep(duration)
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Runtime for &'static NativeRuntime {
    fn spawn<F>(&self, f: F)
    where
        F: Future<Output = ()> + WasmNotSend + 'static,
    {
        (*self).spawn(f);
    }

    fn sleep(&self, duration: Duration) -> impl Future<Output = ()> + WasmNotSend {
        (*self).sleep(duration)
    }
}

#[cfg(target_arch = "wasm32")]
pub trait WasmNotSend {}
#[cfg(target_arch = "wasm32")]
impl<T> WasmNotSend for T {}

#[cfg(not(target_arch = "wasm32"))]
pub trait WasmNotSend: Send {}
#[cfg(not(target_arch = "wasm32"))]
impl<T: Send> WasmNotSend for T {}
