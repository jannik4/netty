use super::{AsyncTransport, ClientTransport, MaxPacketSize, Transport, TransportError};
use crate::NetworkError;
use std::{
    convert::Infallible,
    io::{self, ErrorKind},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::Mutex,
};
use webtransport_web_sys::{WebTransport, WebTransportReceiveStream, WebTransportSendStream};

#[derive(Debug)]
pub struct WebTransportClientTransport {
    _connection: WebTransport,
    recv: Mutex<WebTransportReceiveStream>,
    send: Mutex<WebTransportSendStream>,
}

impl WebTransportClientTransport {
    pub fn connect(url: impl ToString) -> AsyncTransport<Self> {
        let url = url.to_string();
        AsyncTransport::new(|_| async move {
            let connection = WebTransport::connect(&url)
                .await
                .map_err(|err| NetworkError::TransportConnect(Box::new(err)))?;
            let bi = connection
                .open_bi()
                .await
                .map_err(|err| NetworkError::TransportConnect(Box::new(err)))?;
            let send_stream =
                bi.send().map_err(|err| NetworkError::TransportConnect(Box::new(err)))?;
            let recv_stream =
                bi.receive().map_err(|err| NetworkError::TransportConnect(Box::new(err)))?;

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
