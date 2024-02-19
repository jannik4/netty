mod channel;
#[cfg(not(target_arch = "wasm32"))]
mod tcp;
#[cfg(not(target_arch = "wasm32"))]
mod udp;
#[cfg(all(feature = "webtransport", not(target_arch = "wasm32")))]
mod webtransport;

use crate::{NettyFuture, Result, Runtime, WasmNotSend};
use std::{convert::Infallible, fmt::Debug, future::Future, hash::Hash, ops::Deref, sync::Arc};
use thiserror::Error;
use tokio::sync::Mutex;

pub use self::channel::{ChannelClientTransport, ChannelServerTransport};

#[cfg(not(target_arch = "wasm32"))]
pub use self::{
    tcp::{TcpClientTransport, TcpServerTransport, TcpServerTransportAddress},
    udp::{UdpClientTransport, UdpServerTransport},
};

#[cfg(all(feature = "webtransport", not(target_arch = "wasm32")))]
pub use self::webtransport::{
    WebTransportClientTransport, WebTransportServerTransport, WebTransportServerTransportAddress,
};

#[derive(Debug, Error)]
pub enum TransportError<C> {
    /// Unrecoverable disconnect.
    #[error("disconnected")]
    Disconnected,
    /// Unrecoverable disconnect to remote.
    #[error("remote disconnected")]
    RemoteDisconnected(C),
    /// Timed out.
    #[error("timed out")]
    TimedOut,
    /// Internal error.
    #[error("transport")]
    Internal(#[source] Box<dyn std::error::Error + Send + Sync>),
}

pub trait ServerTransport: Transport {
    type ConnectionId: Debug + Copy + PartialEq + Eq + Hash + Send + Sync + 'static;

    fn send_to(
        &self,
        buf: &[u8],
        id: Self::ConnectionId,
    ) -> impl Future<Output = Result<(), TransportError<()>>> + WasmNotSend;
    fn recv_from(
        &self,
        buf: &mut [u8],
    ) -> impl Future<Output = Result<(usize, Self::ConnectionId), TransportError<Self::ConnectionId>>>
           + WasmNotSend;
    fn cleanup(&self, id: Self::ConnectionId) -> impl Future<Output = ()> + WasmNotSend;
}

pub trait ClientTransport: Transport {
    fn send(
        &self,
        buf: &[u8],
    ) -> impl Future<Output = Result<(), TransportError<Infallible>>> + WasmNotSend;
    fn recv(
        &self,
        buf: &mut [u8],
    ) -> impl Future<Output = Result<usize, TransportError<Infallible>>> + WasmNotSend;
}

pub trait Transport {
    // TODO: Remove these constants
    const IS_RELIABLE: bool;
    const IS_ORDERED: bool;
    const MAX_PACKET_SIZE: MaxPacketSize;

    const TRANSPORT_PROPERTIES: TransportProperties = TransportProperties {
        is_reliable: Self::IS_RELIABLE,
        is_ordered: Self::IS_ORDERED,
        max_packet_size: Self::MAX_PACKET_SIZE,
    };
}

#[derive(Debug, Clone, Copy)]
pub struct TransportProperties {
    pub is_reliable: bool,
    pub is_ordered: bool,
    pub max_packet_size: MaxPacketSize, // TODO: Make this a function, so the transport can change it at runtime ???
}

pub struct AsyncTransport<T>(
    Box<dyn FnOnce(Arc<dyn Runtime>) -> NettyFuture<Result<T>> + Send + Sync + 'static>,
);

impl<T> AsyncTransport<T> {
    pub fn new<F>(f: impl FnOnce(Arc<dyn Runtime>) -> F + Send + Sync + 'static) -> Self
    where
        F: Future<Output = Result<T>> + WasmNotSend + 'static,
    {
        Self(Box::new(|runtime| Box::pin(f(runtime))))
    }

    pub async fn start(self, runtime: Arc<dyn Runtime>) -> Result<T> {
        (self.0)(runtime).await
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MaxPacketSize(usize);

impl MaxPacketSize {
    pub const MAX: Self = MaxPacketSize(2usize.pow(20)); // 1 MB

    pub const fn new(size: usize) -> Self {
        if size >= 256 && size <= Self::MAX.0 {
            Self(size)
        } else {
            panic!("invalid max packet size");
        }
    }

    pub const fn get(&self) -> usize {
        self.0
    }
}

impl Deref for MaxPacketSize {
    type Target = usize;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

pub struct ReconnectOnDisconnectClientTransport<T> {
    transport: Mutex<T>,
    reconnect: Box<dyn Fn() -> Result<T, Box<dyn std::error::Error + Send + Sync>> + Send + Sync>,
}

impl<T> ReconnectOnDisconnectClientTransport<T>
where
    T: Transport,
{
    pub fn new(
        transport: T,
        reconnect: impl Fn() -> Result<T, Box<dyn std::error::Error + Send + Sync>>
            + Send
            + Sync
            + 'static,
    ) -> Self {
        Self { transport: Mutex::new(transport), reconnect: Box::new(reconnect) }
    }
}

impl<T> Transport for ReconnectOnDisconnectClientTransport<T>
where
    T: Transport,
{
    const IS_RELIABLE: bool = false; // this is always false, since reconnecting makes it unreliable
    const IS_ORDERED: bool = false; // this is always false, since reconnecting makes it unreliable
    const MAX_PACKET_SIZE: MaxPacketSize = T::MAX_PACKET_SIZE;
}

impl<T> ClientTransport for ReconnectOnDisconnectClientTransport<T>
where
    T: ClientTransport + Send + Sync + 'static,
{
    async fn send(&self, buf: &[u8]) -> Result<(), TransportError<Infallible>> {
        let err = match self.transport.lock().await.send(buf).await {
            Ok(()) => return Ok(()),
            Err(err) => err,
        };

        match err {
            TransportError::Disconnected => {
                let mut transport = self.transport.lock().await;
                *transport = match (self.reconnect)() {
                    Ok(transport) => transport,
                    Err(err) => return Err(TransportError::Internal(err)),
                };
                Err(TransportError::Internal("reconnected after disconnect".into()))
            }
            TransportError::RemoteDisconnected(never) => match never {},
            TransportError::TimedOut => Err(TransportError::TimedOut),
            TransportError::Internal(err) => Err(TransportError::Internal(err)),
        }
    }

    async fn recv(&self, buf: &mut [u8]) -> Result<usize, TransportError<Infallible>> {
        let err = match self.transport.lock().await.recv(buf).await {
            Ok(size) => return Ok(size),
            Err(err) => err,
        };

        match err {
            TransportError::Disconnected => {
                let mut transport = self.transport.lock().await;
                *transport = match (self.reconnect)() {
                    Ok(transport) => transport,
                    Err(err) => return Err(TransportError::Internal(err)),
                };
                Err(TransportError::Internal("reconnected after disconnect".into()))
            }
            TransportError::RemoteDisconnected(never) => match never {},
            TransportError::TimedOut => Err(TransportError::TimedOut),
            TransportError::Internal(err) => Err(TransportError::Internal(err)),
        }
    }
}
