use super::{ClientTransport, MaxPacketSize, ServerTransport, Transport, TransportError};
use std::{
    convert::Infallible,
    io::ErrorKind,
    net::{SocketAddr, UdpSocket},
    time::Duration,
};

#[derive(Debug)]
pub struct UdpServerTransport {
    socket: UdpSocket,
}

impl UdpServerTransport {
    pub fn bind(local_addr: SocketAddr) -> crate::Result<Self> {
        let socket = UdpSocket::bind(local_addr)?;
        socket.set_read_timeout(Some(Duration::from_secs(5)))?;
        socket.set_write_timeout(Some(Duration::from_secs(5)))?;

        Ok(Self { socket })
    }
}

impl Transport for UdpServerTransport {
    const IS_RELIABLE: bool = false;
    const IS_ORDERED: bool = false;
    const MAX_PACKET_SIZE: MaxPacketSize = MaxPacketSize::new(256); // TODO: ???
}

impl ServerTransport for UdpServerTransport {
    type ConnectionId = SocketAddr;

    fn send_to(&self, buf: &[u8], id: Self::ConnectionId) -> Result<(), TransportError<()>> {
        match self.socket.send_to(buf, id) {
            Ok(size) if size == buf.len() => Ok(()),
            Ok(_) => Err(TransportError::Internal(Box::new(std::io::Error::new(
                ErrorKind::Other,
                "failed to send entire buffer",
            )))),
            Err(e) => match e.kind() {
                ErrorKind::WouldBlock | ErrorKind::TimedOut => Err(TransportError::TimedOut),
                _ => Err(TransportError::Internal(Box::new(e))),
            },
        }
    }

    fn recv_from(
        &self,
        buf: &mut [u8],
    ) -> Result<(usize, Self::ConnectionId), TransportError<Self::ConnectionId>> {
        match self.socket.recv_from(buf) {
            Ok((size, addr)) => Ok((size, addr)),
            Err(e) => match e.kind() {
                ErrorKind::WouldBlock | ErrorKind::TimedOut => Err(TransportError::TimedOut),
                _ => Err(TransportError::Internal(Box::new(e))),
            },
        }
    }

    fn cleanup(&self, _id: Self::ConnectionId) {
        // Nothing to do, since UDP is connectionless
    }
}

#[derive(Debug)]
pub struct UdpClientTransport {
    socket: UdpSocket,
}

impl UdpClientTransport {
    pub fn connect(local_addr: SocketAddr, remote_addr: SocketAddr) -> crate::Result<Self> {
        let socket = UdpSocket::bind(local_addr)?;
        socket.connect(remote_addr)?;
        socket.set_read_timeout(Some(Duration::from_secs(5)))?;
        socket.set_write_timeout(Some(Duration::from_secs(5)))?;

        Ok(Self { socket })
    }
}

impl Transport for UdpClientTransport {
    const IS_RELIABLE: bool = false;
    const IS_ORDERED: bool = false;
    const MAX_PACKET_SIZE: MaxPacketSize = MaxPacketSize::new(256); // TODO: ???
}

impl ClientTransport for UdpClientTransport {
    fn send(&self, buf: &[u8]) -> Result<(), TransportError<Infallible>> {
        match self.socket.send(buf) {
            Ok(size) if size == buf.len() => Ok(()),
            Ok(_) => Err(TransportError::Internal(Box::new(std::io::Error::new(
                ErrorKind::Other,
                "failed to send entire buffer",
            )))),
            Err(e) => match e.kind() {
                ErrorKind::WouldBlock | ErrorKind::TimedOut => Err(TransportError::TimedOut),
                _ => Err(TransportError::Internal(Box::new(e))),
            },
        }
    }

    fn recv(&self, buf: &mut [u8]) -> Result<usize, TransportError<Infallible>> {
        match self.socket.recv(buf) {
            Ok(size) => Ok(size),
            Err(e) => match e.kind() {
                ErrorKind::WouldBlock | ErrorKind::TimedOut => Err(TransportError::TimedOut),
                _ => Err(TransportError::Internal(Box::new(e))),
            },
        }
    }
}
