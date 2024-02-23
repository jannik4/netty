use super::{
    AsyncTransport, ClientTransport, MaxPacketSize, ServerTransport, Transport, TransportError,
};
use crate::{NetworkError, Runtime, RuntimeExt};
use std::{
    collections::HashMap,
    convert::Infallible,
    io::{self, ErrorKind},
    net::SocketAddr,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::{
        mpsc::{self, UnboundedReceiver, UnboundedSender},
        oneshot, Mutex, RwLock,
    },
};
use wtransport::{
    endpoint::endpoint_side::Server, Certificate, ClientConfig, Connection, Endpoint, RecvStream,
    SendStream, ServerConfig,
};

#[derive(Debug)]
pub struct WebTransportServerTransport {
    is_alive: Arc<AtomicBool>,
    connections: Arc<RwLock<HashMap<usize, SendStream>>>,
    receiver: Mutex<UnboundedReceiver<(usize, Result<Box<[u8]>, ()>)>>,
}

impl Drop for WebTransportServerTransport {
    fn drop(&mut self) {
        self.is_alive.store(false, Ordering::Relaxed);
    }
}

impl WebTransportServerTransport {
    pub fn bind(
        local_addr: SocketAddr,
        certificate: WebTransportServerTransportCertificate,
    ) -> (WebTransportServerTransportAddress, AsyncTransport<Self>) {
        let (local_addr_sender, local_addr_receiver) = oneshot::channel();

        (
            WebTransportServerTransportAddress(local_addr_receiver),
            AsyncTransport::new(move |runtime: Arc<dyn Runtime>| async move {
                let is_alive = Arc::new(AtomicBool::new(true));

                let certificate = match certificate {
                    WebTransportServerTransportCertificate::Load { cert, key } => {
                        Certificate::load(cert, key)
                            .await
                            .map_err(|err| NetworkError::TransportConnect(Box::new(err)))?
                    }
                    WebTransportServerTransportCertificate::SelfSigned => {
                        Certificate::self_signed::<&[&str], _>(&[])
                    }
                };
                let hashes = certificate.hashes().into_iter().map(|hash| *hash.as_ref()).collect();

                let config = ServerConfig::builder()
                    .with_bind_address(local_addr)
                    .with_certificate(certificate)
                    .build();

                let endpoint = Endpoint::server(config)?;
                local_addr_sender.send((endpoint.local_addr()?, hashes)).ok();
                let connections = Arc::new(RwLock::new(HashMap::new()));
                let (sender, receiver) = mpsc::unbounded_channel();

                runtime.spawn({
                    let is_alive = Arc::clone(&is_alive);
                    let connections = Arc::clone(&connections);
                    let runtime = Arc::clone(&runtime);
                    async move {
                        Self::accept(is_alive, endpoint, connections, sender, runtime).await;
                    }
                });

                Ok(Self { is_alive, connections, receiver: Mutex::new(receiver) })
            }),
        )
    }

    async fn accept(
        is_alive: Arc<AtomicBool>,
        endpoint: Endpoint<Server>,
        connections: Arc<RwLock<HashMap<usize, SendStream>>>,
        sender: UnboundedSender<(usize, Result<Box<[u8]>, ()>)>,
        runtime: Arc<dyn Runtime>,
    ) {
        while is_alive.load(Ordering::Relaxed) {
            let incoming = endpoint.accept().await;

            runtime.spawn({
                let is_alive = Arc::clone(&is_alive);
                let connections = Arc::clone(&connections);
                let sender = sender.clone();
                async move {
                    let session_request = match incoming.await {
                        Ok(session_request) => session_request,
                        Err(_e) => {
                            println!("ERROR: {_e:?}"); // TODO: REMOVE ME

                            // TODO: Send/Handle error to user
                            return;
                        }
                    };
                    let connection = match session_request.accept().await {
                        Ok(connection) => connection,
                        Err(_e) => {
                            println!("ERROR: {_e:?}"); // TODO: REMOVE ME

                            // TODO: Send/Handle error to user
                            return;
                        }
                    };

                    let (send_stream, mut recv_stream) = match connection.accept_bi().await {
                        Ok(streams) => streams,
                        Err(_e) => {
                            println!("ERROR: {_e:?}"); // TODO: REMOVE ME

                            // TODO: Send/Handle error to user
                            return;
                        }
                    };

                    connections.write().await.insert(connection.stable_id(), send_stream);

                    let mut buf = vec![0; Self::MAX_PACKET_SIZE.get()];
                    while is_alive.load(Ordering::Relaxed)
                        && connections.read().await.contains_key(&connection.stable_id())
                    {
                        match impl_recv(&mut recv_stream, &mut buf).await {
                            Ok(size) => {
                                sender.send((connection.stable_id(), Ok(buf[0..size].into()))).ok();
                            }
                            Err(e) => match e.kind() {
                                ErrorKind::WouldBlock | ErrorKind::TimedOut => (),
                                _ => {
                                    // TODO: Recover possible?
                                    connections.write().await.remove(&connection.stable_id());
                                    sender.send((connection.stable_id(), Err(()))).ok();
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

impl Transport for WebTransportServerTransport {
    const IS_RELIABLE: bool = true;
    const IS_ORDERED: bool = true;
    const MAX_PACKET_SIZE: MaxPacketSize = MaxPacketSize::MAX;
}

impl ServerTransport for WebTransportServerTransport {
    type ConnectionId = usize;

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
pub enum WebTransportServerTransportCertificate {
    SelfSigned,
    Load { cert: PathBuf, key: PathBuf },
}

#[derive(Debug)]
pub struct WebTransportServerTransportAddress(oneshot::Receiver<(SocketAddr, Vec<[u8; 32]>)>);

impl WebTransportServerTransportAddress {
    pub async fn get(self) -> Option<(SocketAddr, Vec<[u8; 32]>)> {
        self.0.await.ok()
    }

    pub fn try_get(&mut self) -> Option<(SocketAddr, Vec<[u8; 32]>)> {
        self.0.try_recv().ok()
    }
}

#[derive(Debug)]
pub struct WebTransportClientTransport {
    _connection: Connection,
    recv: Mutex<RecvStream>,
    send: Mutex<SendStream>,
}

impl WebTransportClientTransport {
    pub fn connect(
        url: impl ToString,
        cert_validation: WebTransportClientTransportCertificateValidation,
    ) -> AsyncTransport<Self> {
        let url = url.to_string();
        AsyncTransport::new(|_| async {
            let config = ClientConfig::builder().with_bind_default();
            let config = match cert_validation {
                WebTransportClientTransportCertificateValidation::UnsecureNoValidation => {
                    config.with_no_cert_validation()
                }
                WebTransportClientTransportCertificateValidation::CertificateHashes(hashes) => {
                    config.with_server_certificate_hashes(hashes.into_iter().map(From::from))
                }
                WebTransportClientTransportCertificateValidation::NativeCerts => {
                    config.with_native_certs()
                }
            };
            let config = config.build();
            let connection = Endpoint::client(config)?
                .connect(url)
                .await
                .map_err(|err| NetworkError::TransportConnect(Box::new(err)))?;
            let (send_stream, recv_stream) = connection
                .open_bi()
                .await
                .map_err(|err| NetworkError::TransportConnect(Box::new(err)))?
                .await
                .map_err(|err| NetworkError::TransportConnect(Box::new(err)))?;

            Ok(Self {
                _connection: connection,
                recv: Mutex::new(recv_stream),
                send: Mutex::new(send_stream),
            })
        })
    }
}

impl Transport for WebTransportClientTransport {
    const IS_RELIABLE: bool = true;
    const IS_ORDERED: bool = true;
    const MAX_PACKET_SIZE: MaxPacketSize = MaxPacketSize::MAX;
}

impl ClientTransport for WebTransportClientTransport {
    async fn send(&self, buf: &[u8]) -> Result<(), TransportError<Infallible>> {
        match impl_send(&mut *self.send.lock().await, buf).await {
            Ok(()) => Ok(()),
            Err(_e) => {
                println!("ERROR: {_e:?}"); // TODO: REMOVE ME

                Err(TransportError::Disconnected) // TODO: Recover possible?
            }
        }
    }

    async fn recv(&self, buf: &mut [u8]) -> Result<usize, TransportError<Infallible>> {
        match impl_recv(&mut *self.recv.lock().await, buf).await {
            Ok(size) => Ok(size),
            Err(_e) => {
                println!("ERROR: {_e:?}"); // TODO: REMOVE ME

                Err(TransportError::Disconnected) // TODO: Recover possible?
            }
        }
    }
}

#[derive(Debug)]
pub enum WebTransportClientTransportCertificateValidation {
    UnsecureNoValidation,
    CertificateHashes(Vec<[u8; 32]>),
    NativeCerts,
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
