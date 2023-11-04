use super::{ClientTransport, MaxPacketSize, ServerTransport, Transport, TransportError};
use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};
use std::{
    collections::HashMap,
    convert::Infallible,
    io::{self, ErrorKind, Read, Write},
    net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, RwLock,
    },
    thread,
    time::Duration,
};

// TODO: Writes on one socket block all other sockets

#[derive(Debug)]
pub struct TcpServerTransport {
    is_alive: Arc<AtomicBool>,
    connections: Arc<RwLock<HashMap<SocketAddr, TcpStream>>>,
    receiver: Receiver<(SocketAddr, Result<Box<[u8]>, ()>)>,
}

impl Drop for TcpServerTransport {
    fn drop(&mut self) {
        self.is_alive.store(false, Ordering::Relaxed);
    }
}

impl TcpServerTransport {
    pub fn bind<A: ToSocketAddrs>(local_addr: A) -> crate::Result<(SocketAddr, Self)> {
        let is_alive = Arc::new(AtomicBool::new(true));

        let listener = TcpListener::bind(local_addr)?;
        let local_addr = listener.local_addr()?;
        listener.set_nonblocking(true)?;
        let connections = Arc::new(RwLock::new(HashMap::new()));
        let (sender, receiver) = crossbeam_channel::unbounded();

        thread::spawn({
            let is_alive = Arc::clone(&is_alive);
            let connections = Arc::clone(&connections);
            move || Self::accept(is_alive, listener, connections, sender)
        });

        Ok((local_addr, Self { is_alive, connections, receiver }))
    }

    fn accept(
        is_alive: Arc<AtomicBool>,
        listener: TcpListener,
        connections: Arc<RwLock<HashMap<SocketAddr, TcpStream>>>,
        sender: Sender<(SocketAddr, Result<Box<[u8]>, ()>)>,
    ) {
        while is_alive.load(Ordering::Relaxed) {
            let (stream, addr) = match listener.accept() {
                Ok((stream, addr)) => (stream, addr),
                Err(e) => match e.kind() {
                    ErrorKind::WouldBlock | ErrorKind::TimedOut => {
                        // TODO: wait for fd or timeout 5s instead of buys loop with 100ms sleep
                        thread::sleep(Duration::from_millis(100));
                        continue;
                    }
                    _ => {
                        // TODO: send error in channel so that recv_from can return it
                        thread::sleep(Duration::from_millis(100));
                        continue;
                    }
                },
            };

            let setup = move || {
                stream.set_nonblocking(false)?;
                stream.set_read_timeout(Some(Duration::from_secs(1)))?;
                stream.set_write_timeout(Some(Duration::from_secs(1)))?;

                Ok::<_, io::Error>((stream.try_clone()?, stream))
            };
            let (stream_recv, stream_send) = match setup() {
                Ok((stream_recv, stream_send)) => (stream_recv, stream_send),
                Err(_e) => {
                    // TODO: send error in channel so that recv_from can return it
                    continue;
                }
            };

            connections.write().unwrap().insert(addr, stream_send);

            thread::spawn({
                let is_alive = Arc::clone(&is_alive);
                let mut stream = stream_recv;
                let sender = sender.clone();
                let connections = Arc::clone(&connections);
                move || {
                    let mut buf = vec![0; Self::MAX_PACKET_SIZE.get()];
                    while is_alive.load(Ordering::Relaxed)
                        && connections.read().unwrap().contains_key(&addr)
                    {
                        match impl_recv(&mut stream, &mut buf) {
                            Ok(size) => {
                                sender.send((addr, Ok(buf[0..size].into()))).ok();
                            }
                            Err(e) => match e.kind() {
                                ErrorKind::WouldBlock | ErrorKind::TimedOut => (),
                                _ => {
                                    // TODO: Recover possible?
                                    connections.write().unwrap().remove(&addr);
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

    fn send_to(&self, buf: &[u8], id: Self::ConnectionId) -> Result<(), TransportError<()>> {
        let mut connections = self.connections.write().unwrap();

        let stream = match connections.get_mut(&id) {
            Some(stream) => stream,
            None => return Err(TransportError::RemoteDisconnected(())),
        };

        match impl_send(stream, buf) {
            Ok(()) => Ok(()),
            Err(_) => {
                // TODO: Recover possible?
                connections.remove(&id);
                Err(TransportError::RemoteDisconnected(()))
            }
        }
    }

    fn recv_from(
        &self,
        buf: &mut [u8],
    ) -> Result<(usize, Self::ConnectionId), TransportError<Self::ConnectionId>> {
        // Receive
        match self.receiver.recv_timeout(Duration::from_secs(5)) {
            Ok((addr, recv)) => match recv {
                Ok(data) => {
                    buf[..data.len()].copy_from_slice(&data);
                    Ok((data.len(), addr))
                }
                Err(()) => {
                    self.connections.write().unwrap().remove(&addr);
                    Err(TransportError::RemoteDisconnected(addr))
                }
            },
            Err(RecvTimeoutError::Timeout) => Err(TransportError::TimedOut),
            Err(RecvTimeoutError::Disconnected) => Err(TransportError::Disconnected),
        }
    }

    fn cleanup(&self, id: Self::ConnectionId) {
        self.connections.write().unwrap().remove(&id);
    }
}

#[derive(Debug)]
pub struct TcpClientTransport {
    recv: Mutex<TcpStream>,
    send: Mutex<TcpStream>,
}

impl TcpClientTransport {
    pub fn connect<A: ToSocketAddrs>(remote_addr: A) -> crate::Result<Self> {
        let recv = TcpStream::connect(remote_addr)?;
        let send = recv.try_clone()?;

        Ok(Self { recv: Mutex::new(recv), send: Mutex::new(send) })
    }
}

impl Transport for TcpClientTransport {
    const IS_RELIABLE: bool = true;
    const IS_ORDERED: bool = true;
    const MAX_PACKET_SIZE: MaxPacketSize = MaxPacketSize::MAX;
}

impl ClientTransport for TcpClientTransport {
    fn send(&self, buf: &[u8]) -> Result<(), TransportError<Infallible>> {
        let run = || {
            let mut stream = self.send.lock().unwrap();
            impl_send(&mut stream, buf)
        };

        match run() {
            Ok(()) => Ok(()),
            Err(_) => Err(TransportError::Disconnected), // TODO: Recover possible?
        }
    }

    fn recv(&self, buf: &mut [u8]) -> Result<usize, TransportError<Infallible>> {
        let mut run = || {
            let mut stream = self.recv.lock().unwrap();
            impl_recv(&mut stream, buf)
        };

        match run() {
            Ok(size) => Ok(size),
            Err(_) => Err(TransportError::Disconnected), // TODO: Recover possible?
        }
    }
}

fn impl_send(stream: &mut TcpStream, buf: &[u8]) -> io::Result<()> {
    stream.write_all(&u32::to_le_bytes(buf.len() as u32))?;
    stream.write_all(buf)?;
    Ok(())
}

fn impl_recv(stream: &mut TcpStream, buf: &mut [u8]) -> io::Result<usize> {
    let mut size_buf = [0u8; 4];
    stream.read_exact(&mut size_buf)?;
    let size = u32::from_le_bytes(size_buf) as usize;

    if size > buf.len() {
        return Err(io::Error::new(
            ErrorKind::Other,
            "received packet size is larger than buffer size",
        ));
    }

    stream.read_exact(&mut buf[0..size])?;

    Ok(size)
}
