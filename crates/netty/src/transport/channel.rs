use super::{ClientTransport, MaxPacketSize, ServerTransport, Transport, TransportError};
use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};
use std::{
    collections::HashMap,
    convert::Infallible,
    sync::{Arc, RwLock},
    time::Duration,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ConnectionId(u32);

#[derive(Debug)]
pub struct ChannelServerTransport {
    connections: Arc<RwLock<HashMap<ConnectionId, Sender<Box<[u8]>>>>>,
    receiver: Receiver<(ConnectionId, Box<[u8]>)>,
}

impl ChannelServerTransport {
    pub fn new() -> (Self, ClientFactory) {
        let connections = Arc::new(RwLock::new(HashMap::new()));
        let (client_sender, receiver) = crossbeam_channel::unbounded();

        let this = Self { connections: Arc::clone(&connections), receiver };
        let factory = ClientFactory { connections, next_id: ConnectionId(0), client_sender };

        (this, factory)
    }

    pub fn new_server_client_pair() -> (Self, ChannelClientTransport) {
        let (server, mut factory) = Self::new();
        let client = factory.create_client();

        (server, client)
    }
}

impl Transport for ChannelServerTransport {
    const IS_RELIABLE: bool = true;
    const IS_ORDERED: bool = true;
    const MAX_PACKET_SIZE: MaxPacketSize = MaxPacketSize::MAX;
}

impl ServerTransport for ChannelServerTransport {
    type ConnectionId = ConnectionId;

    fn send_to(&self, buf: &[u8], id: Self::ConnectionId) -> Result<(), TransportError<()>> {
        let connections = self.connections.read().unwrap();

        let sender = match connections.get(&id) {
            Some(sender) => sender,
            None => return Err(TransportError::RemoteDisconnected(())),
        };

        match sender.send(buf.into()) {
            Ok(()) => Ok(()),
            Err(_) => {
                drop(connections); // Unlock the read lock
                self.cleanup(id);
                Err(TransportError::RemoteDisconnected(()))
            }
        }
    }

    fn recv_from(
        &self,
        buf: &mut [u8],
    ) -> Result<(usize, Self::ConnectionId), TransportError<Self::ConnectionId>> {
        match self.receiver.recv_timeout(Duration::from_secs(5)) {
            Ok((id, data)) => {
                let len = data.len().min(buf.len());
                buf[..len].copy_from_slice(&data[..len]);
                Ok((len, id))
            }
            Err(RecvTimeoutError::Timeout) => Err(TransportError::TimedOut),
            Err(RecvTimeoutError::Disconnected) => Err(TransportError::Disconnected),
        }
    }

    fn cleanup(&self, id: Self::ConnectionId) {
        self.connections.write().unwrap().remove(&id);
    }
}

#[derive(Debug)]
pub struct ChannelClientTransport {
    id: ConnectionId,
    sender: Sender<(ConnectionId, Box<[u8]>)>,
    receiver: Receiver<Box<[u8]>>,
}

impl Transport for ChannelClientTransport {
    const IS_RELIABLE: bool = true;
    const IS_ORDERED: bool = true;
    const MAX_PACKET_SIZE: MaxPacketSize = MaxPacketSize::MAX;
}

impl ClientTransport for ChannelClientTransport {
    fn send(&self, buf: &[u8]) -> Result<(), TransportError<Infallible>> {
        match self.sender.send((self.id, buf.into())) {
            Ok(()) => Ok(()),
            Err(_) => Err(TransportError::Disconnected),
        }
    }

    fn recv(&self, buf: &mut [u8]) -> Result<usize, TransportError<Infallible>> {
        match self.receiver.recv_timeout(Duration::from_secs(5)) {
            Ok(data) => {
                let len = data.len().min(buf.len());
                buf[..len].copy_from_slice(&data[..len]);
                Ok(len)
            }
            Err(RecvTimeoutError::Timeout) => Err(TransportError::TimedOut),
            Err(RecvTimeoutError::Disconnected) => Err(TransportError::Disconnected),
        }
    }
}

#[derive(Debug)]
pub struct ClientFactory {
    connections: Arc<RwLock<HashMap<ConnectionId, Sender<Box<[u8]>>>>>,
    next_id: ConnectionId,
    client_sender: Sender<(ConnectionId, Box<[u8]>)>,
}

impl ClientFactory {
    pub fn create_client(&mut self) -> ChannelClientTransport {
        let id = self.next_id;
        self.next_id.0 += 1;

        let (sender, receiver) = crossbeam_channel::unbounded();
        self.connections.write().unwrap().insert(id, sender);

        ChannelClientTransport { id, sender: self.client_sender.clone(), receiver }
    }
}
