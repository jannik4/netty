use super::{
    AsyncTransport, ClientTransport, MaxPacketSize, ServerTransport, Transport, TransportError,
};
use std::{collections::HashMap, convert::Infallible, sync::Arc};
use tokio::sync::{
    mpsc::{self, UnboundedReceiver, UnboundedSender},
    Mutex, RwLock,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ConnectionId(u32);

#[derive(Debug)]
pub struct ChannelServerTransport {
    connections: Arc<RwLock<HashMap<ConnectionId, UnboundedSender<Box<[u8]>>>>>,
    receiver: Mutex<UnboundedReceiver<(ConnectionId, Box<[u8]>)>>,
}

impl ChannelServerTransport {
    pub fn new() -> (AsyncTransport<Self>, ClientFactory) {
        let connections = Arc::new(RwLock::new(HashMap::new()));
        let (client_sender, receiver) = mpsc::unbounded_channel();

        let this = Self { connections: Arc::clone(&connections), receiver: Mutex::new(receiver) };
        let factory = ClientFactory { connections, next_id: ConnectionId(0), client_sender };

        (AsyncTransport::new(|_| async { Ok(this) }), factory)
    }

    pub fn new_server_client_pair() -> (AsyncTransport<Self>, AsyncTransport<ChannelClientTransport>)
    {
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

    async fn send_to(&self, buf: &[u8], id: Self::ConnectionId) -> Result<(), TransportError<()>> {
        let connections = self.connections.read().await;

        let sender = match connections.get(&id) {
            Some(sender) => sender,
            None => return Err(TransportError::RemoteDisconnected(())),
        };

        match sender.send(buf.into()) {
            Ok(()) => Ok(()),
            Err(_) => {
                drop(connections); // Unlock the read lock
                self.cleanup(id).await;
                Err(TransportError::RemoteDisconnected(()))
            }
        }
    }

    async fn recv_from(
        &self,
        buf: &mut [u8],
    ) -> Result<(usize, Self::ConnectionId), TransportError<Self::ConnectionId>> {
        match self.receiver.lock().await.recv().await {
            Some((id, data)) => {
                let len = data.len().min(buf.len());
                buf[..len].copy_from_slice(&data[..len]);
                Ok((len, id))
            }
            None => Err(TransportError::Disconnected),
        }
    }

    async fn cleanup(&self, id: Self::ConnectionId) {
        self.connections.write().await.remove(&id);
    }
}

#[derive(Debug)]
pub struct ChannelClientTransport {
    id: ConnectionId,
    sender: UnboundedSender<(ConnectionId, Box<[u8]>)>,
    receiver: Mutex<UnboundedReceiver<Box<[u8]>>>,
}

impl Transport for ChannelClientTransport {
    const IS_RELIABLE: bool = true;
    const IS_ORDERED: bool = true;
    const MAX_PACKET_SIZE: MaxPacketSize = MaxPacketSize::MAX;
}

impl ClientTransport for ChannelClientTransport {
    async fn send(&self, buf: &[u8]) -> Result<(), TransportError<Infallible>> {
        match self.sender.send((self.id, buf.into())) {
            Ok(()) => Ok(()),
            Err(_) => Err(TransportError::Disconnected),
        }
    }

    async fn recv(&self, buf: &mut [u8]) -> Result<usize, TransportError<Infallible>> {
        match self.receiver.lock().await.recv().await {
            Some(data) => {
                let len = data.len().min(buf.len());
                buf[..len].copy_from_slice(&data[..len]);
                Ok(len)
            }
            None => Err(TransportError::Disconnected),
        }
    }
}

#[derive(Debug)]
pub struct ClientFactory {
    connections: Arc<RwLock<HashMap<ConnectionId, UnboundedSender<Box<[u8]>>>>>,
    next_id: ConnectionId,
    client_sender: UnboundedSender<(ConnectionId, Box<[u8]>)>,
}

impl ClientFactory {
    pub fn create_client(&mut self) -> AsyncTransport<ChannelClientTransport> {
        let id = self.next_id;
        self.next_id.0 += 1;

        let client_sender = self.client_sender.clone();
        let connections = Arc::clone(&self.connections);

        AsyncTransport::new(move |_| async move {
            let (sender, receiver) = mpsc::unbounded_channel();
            connections.write().await.insert(id, sender);

            Ok(ChannelClientTransport { id, sender: client_sender, receiver: Mutex::new(receiver) })
        })
    }
}
