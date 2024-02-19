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

use std::{future::Future, pin::Pin, time::Duration};
use thiserror::Error;

pub use {
    self::{
        client::{AsyncClientTransport, Client, ClientEvent},
        handle::ConnectionHandle,
        server::{AsyncServerTransports, Server, ServerEvent},
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

#[cfg(target_arch = "wasm32")]
pub type NettyFuture<T> = Pin<Box<dyn Future<Output = T> + 'static>>;

#[cfg(not(target_arch = "wasm32"))]
pub type NettyFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

pub trait Runtime: native_only_tokio::NativeOnlyTokio + Send + Sync + 'static {
    fn spawn_boxed(&self, f: NettyFuture<()>);
    fn sleep(&self, duration: Duration) -> NettyFuture<()>;
}

impl Runtime for Box<dyn Runtime> {
    fn spawn_boxed(&self, f: NettyFuture<()>) {
        (**self).spawn_boxed(f);
    }

    fn sleep(&self, duration: Duration) -> NettyFuture<()> {
        (**self).sleep(duration)
    }
}

impl Runtime for std::sync::Arc<dyn Runtime> {
    fn spawn_boxed(&self, f: NettyFuture<()>) {
        (**self).spawn_boxed(f);
    }

    fn sleep(&self, duration: Duration) -> NettyFuture<()> {
        (**self).sleep(duration)
    }
}

pub trait RuntimeExt: Runtime {
    #[cfg(target_arch = "wasm32")]
    fn spawn(&self, f: impl Future<Output = ()> + 'static) {
        self.spawn_boxed(Box::pin(f));
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn spawn(&self, f: impl Future<Output = ()> + Send + 'static) {
        self.spawn_boxed(Box::pin(f));
    }
}

impl<T: Runtime> RuntimeExt for T {}

#[cfg(target_arch = "wasm32")]
mod native_only_tokio {
    pub trait NativeOnlyTokio {}

    impl<T> NativeOnlyTokio for T {}
}

#[cfg(not(target_arch = "wasm32"))]
mod native_only_tokio {
    pub trait NativeOnlyTokio {}

    impl NativeOnlyTokio for tokio::runtime::Runtime {}
    impl NativeOnlyTokio for &'static tokio::runtime::Runtime {}
    impl NativeOnlyTokio for Box<dyn super::Runtime> {}
    impl NativeOnlyTokio for std::sync::Arc<dyn super::Runtime> {}
}

#[cfg(not(target_arch = "wasm32"))]
impl Runtime for tokio::runtime::Runtime {
    fn spawn_boxed(&self, f: NettyFuture<()>) {
        self.spawn(f);
    }
    fn sleep(&self, duration: Duration) -> NettyFuture<()> {
        Box::pin(tokio::time::sleep(duration))
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Runtime for &'static tokio::runtime::Runtime {
    fn spawn_boxed(&self, f: NettyFuture<()>) {
        (*self).spawn_boxed(f);
    }
    fn sleep(&self, duration: Duration) -> NettyFuture<()> {
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
