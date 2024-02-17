use super::{
    AsyncTransport, ClientTransport, MaxPacketSize, ServerTransport, Transport, TransportError,
};
use crate::Runtime;
use std::{
    collections::HashMap,
    convert::Infallible,
    io::{self, ErrorKind},
    net::SocketAddr,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{
        tcp::{OwnedReadHalf, OwnedWriteHalf},
        TcpListener, TcpStream, ToSocketAddrs,
    },
    sync::{
        mpsc::{self, UnboundedReceiver, UnboundedSender},
        oneshot, Mutex, RwLock,
    },
};

// TODO: Writes on one socket block all other sockets
// TODO: It can take up to 1 second for the listener to shudown after calling disconnect

#[derive(Debug)]
pub struct TcpServerTransport {
    is_alive: Arc<AtomicBool>,
    connections: Arc<RwLock<HashMap<SocketAddr, OwnedWriteHalf>>>,
    receiver: Mutex<UnboundedReceiver<(SocketAddr, Result<Box<[u8]>, ()>)>>,
}

impl Drop for TcpServerTransport {
    fn drop(&mut self) {
        self.is_alive.store(false, Ordering::Relaxed);
    }
}

impl TcpServerTransport {
    pub fn bind<A: ToSocketAddrs + Send + 'static, R: Runtime>(
        local_addr: A,
    ) -> (TcpServerTransportAddress, AsyncTransport<Self, R>) {
        let (local_addr_sender, local_addr_receiver) = oneshot::channel();

        (
            TcpServerTransportAddress(local_addr_receiver),
            AsyncTransport::new(move |runtime: Arc<R>| async move {
                let is_alive = Arc::new(AtomicBool::new(true));

                let listener = TcpListener::bind(local_addr).await?;
                local_addr_sender.send(listener.local_addr()?).ok();
                let connections = Arc::new(RwLock::new(HashMap::new()));
                let (sender, receiver) = mpsc::unbounded_channel();

                runtime.spawn({
                    let is_alive = Arc::clone(&is_alive);
                    let connections = Arc::clone(&connections);
                    let runtime = Arc::clone(&runtime);
                    async move {
                        Self::accept(is_alive, listener, connections, sender, runtime).await;
                    }
                });

                Ok(Self { is_alive, connections, receiver: Mutex::new(receiver) })
            }),
        )
    }

    async fn accept<R: Runtime>(
        is_alive: Arc<AtomicBool>,
        listener: TcpListener,
        connections: Arc<RwLock<HashMap<SocketAddr, OwnedWriteHalf>>>,
        sender: UnboundedSender<(SocketAddr, Result<Box<[u8]>, ()>)>,
        runtime: Arc<R>,
    ) {
        while is_alive.load(Ordering::Relaxed) {
            let (stream, addr) = match listener.accept().await {
                Ok((stream, addr)) => (stream, addr),
                Err(_e) => {
                    // TODO: send error in channel so that recv_from can return it
                    runtime.sleep(Duration::from_millis(100));
                    continue;
                }
            };

            let (stream_recv, stream_send) = stream.into_split();

            connections.write().await.insert(addr, stream_send);

            runtime.spawn({
                let is_alive = Arc::clone(&is_alive);
                let mut stream = stream_recv;
                let sender = sender.clone();
                let connections = Arc::clone(&connections);
                async move {
                    let mut buf = vec![0; Self::MAX_PACKET_SIZE.get()];
                    while is_alive.load(Ordering::Relaxed)
                        && connections.read().await.contains_key(&addr)
                    {
                        match impl_recv(&mut stream, &mut buf).await {
                            Ok(size) => {
                                sender.send((addr, Ok(buf[0..size].into()))).ok();
                            }
                            Err(e) => match e.kind() {
                                ErrorKind::WouldBlock | ErrorKind::TimedOut => (),
                                _ => {
                                    // TODO: Recover possible?
                                    connections.write().await.remove(&addr);
                                    sender.send((addr, Err(()))).ok();
                                    break;
                                }
                            },
                        }
                    }
                }
            });
        }
    }
}

impl Transport for TcpServerTransport {
    const IS_RELIABLE: bool = true;
    const IS_ORDERED: bool = true;
    const MAX_PACKET_SIZE: MaxPacketSize = MaxPacketSize::MAX;
}

impl ServerTransport for TcpServerTransport {
    type ConnectionId = SocketAddr;

    async fn send_to(&self, buf: &[u8], id: Self::ConnectionId) -> Result<(), TransportError<()>> {
        let mut connections = self.connections.write().await;

        let stream = match connections.get_mut(&id) {
            Some(stream) => stream,
            None => return Err(TransportError::RemoteDisconnected(())),
        };

        match impl_send(stream, buf).await {
            Ok(()) => Ok(()),
            Err(_) => {
                // TODO: Recover possible?
                connections.remove(&id);
                Err(TransportError::RemoteDisconnected(()))
            }
        }
    }

    async fn recv_from(
        &self,
        buf: &mut [u8],
    ) -> Result<(usize, Self::ConnectionId), TransportError<Self::ConnectionId>> {
        // Receive
        match self.receiver.lock().await.recv().await {
            Some((addr, recv)) => match recv {
                Ok(data) => {
                    buf[..data.len()].copy_from_slice(&data);
                    Ok((data.len(), addr))
                }
                Err(()) => {
                    self.connections.write().await.remove(&addr);
                    Err(TransportError::RemoteDisconnected(addr))
                }
            },
            None => Err(TransportError::Disconnected),
        }
    }

    async fn cleanup(&self, id: Self::ConnectionId) {
        self.connections.write().await.remove(&id);
    }
}

#[derive(Debug)]
pub struct TcpServerTransportAddress(oneshot::Receiver<SocketAddr>);

impl TcpServerTransportAddress {
    pub async fn get(self) -> Option<SocketAddr> {
        self.0.await.ok()
    }

    pub fn try_get(&mut self) -> Option<SocketAddr> {
        self.0.try_recv().ok()
    }
}

#[derive(Debug)]
pub struct TcpClientTransport {
    recv: Mutex<OwnedReadHalf>,
    send: Mutex<OwnedWriteHalf>,
}

impl TcpClientTransport {
    pub fn connect<A: ToSocketAddrs + Send + 'static, R>(
        remote_addr: A,
    ) -> AsyncTransport<Self, R> {
        AsyncTransport::new(|_| async {
            let stream = TcpStream::connect(remote_addr).await?;
            let (recv, send) = stream.into_split();

            Ok(Self { recv: Mutex::new(recv), send: Mutex::new(send) })
        })
    }
}

impl Transport for TcpClientTransport {
    const IS_RELIABLE: bool = true;
    const IS_ORDERED: bool = true;
    const MAX_PACKET_SIZE: MaxPacketSize = MaxPacketSize::MAX;
}

impl ClientTransport for TcpClientTransport {
    async fn send(&self, buf: &[u8]) -> Result<(), TransportError<Infallible>> {
        match impl_send(&mut *self.send.lock().await, buf).await {
            Ok(()) => Ok(()),
            Err(_) => Err(TransportError::Disconnected), // TODO: Recover possible?
        }
    }

    async fn recv(&self, buf: &mut [u8]) -> Result<usize, TransportError<Infallible>> {
        match impl_recv(&mut *self.recv.lock().await, buf).await {
            Ok(size) => Ok(size),
            Err(_) => Err(TransportError::Disconnected), // TODO: Recover possible?
        }
    }
}

async fn impl_send<S>(stream: &mut S, buf: &[u8]) -> io::Result<()>
where
    S: AsyncWriteExt + Unpin,
{
    stream.write_all(&u32::to_le_bytes(buf.len() as u32)).await?;
    stream.write_all(buf).await?;
    Ok(())
}

async fn impl_recv<S>(stream: &mut S, buf: &mut [u8]) -> io::Result<usize>
where
    S: AsyncReadExt + Unpin,
{
    let mut size_buf = [0u8; 4];
    stream.read_exact(&mut size_buf).await?;
    let size = u32::from_le_bytes(size_buf) as usize;

    if size > buf.len() {
        return Err(io::Error::new(
            ErrorKind::Other,
            "received packet size is larger than buffer size",
        ));
    }

    stream.read_exact(&mut buf[0..size]).await?;

    Ok(size)
}
