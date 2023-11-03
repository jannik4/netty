mod channel;
mod tcp;
mod udp;

use std::{convert::Infallible, fmt::Debug, hash::Hash, ops::Deref, sync::Mutex};
use thiserror::Error;

pub use self::{
    channel::{ChannelClientTransport, ChannelServerTransport},
    tcp::{TcpClientTransport, TcpServerTransport},
    udp::{UdpClientTransport, UdpServerTransport},
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

    fn send_to(&self, buf: &[u8], id: Self::ConnectionId) -> Result<(), TransportError<()>>;
    fn recv_from(
        &self,
        buf: &mut [u8],
    ) -> Result<(usize, Self::ConnectionId), TransportError<Self::ConnectionId>>;
    fn cleanup(&self, id: Self::ConnectionId);
}

pub trait ClientTransport: Transport {
    fn send(&self, buf: &[u8]) -> Result<(), TransportError<Infallible>>;
    fn recv(&self, buf: &mut [u8]) -> Result<usize, TransportError<Infallible>>;
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
    T: ClientTransport,
{
    fn send(&self, buf: &[u8]) -> Result<(), TransportError<Infallible>> {
        let err = match self.transport.lock().unwrap().send(buf) {
            Ok(()) => return Ok(()),
            Err(err) => err,
        };

        match err {
            TransportError::Disconnected => {
                let mut transport = self.transport.lock().unwrap();
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

    fn recv(&self, buf: &mut [u8]) -> Result<usize, TransportError<Infallible>> {
        let err = match self.transport.lock().unwrap().recv(buf) {
            Ok(size) => return Ok(size),
            Err(err) => err,
        };

        match err {
            TransportError::Disconnected => {
                let mut transport = self.transport.lock().unwrap();
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
