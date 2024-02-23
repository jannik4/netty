use js_sys::{Array, Object, Reflect, Uint8Array};
use send_wrapper::SendWrapper;
use std::{
    fmt,
    future::Future,
    io,
    pin::Pin,
    task::{ready, Context, Poll},
};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use wasm_bindgen_futures::JsFuture;
use web_sys::wasm_bindgen::JsValue;

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug)]
pub struct WebTransport {
    transport: SendWrapper<web_sys::WebTransport>,
}

impl WebTransport {
    pub async fn connect(url: &str, hashes: Option<Vec<[u8; 32]>>) -> Result<Self> {
        let mut options = web_sys::WebTransportOptions::new();
        if let Some(hashes) = hashes {
            let hashes = hashes
                .into_iter()
                .map(|hash| {
                    let item = Object::new();
                    Reflect::set(&item, &"algorithm".into(), &"sha-256".into());
                    Reflect::set(
                        &item,
                        &"value".into(),
                        &Uint8Array::from(&hash[..]).buffer().into(),
                    );
                    JsValue::from(item)
                })
                .collect::<Array>();
            options.server_certificate_hashes(&hashes);
        }

        let transport = SendWrapper::new(web_sys::WebTransport::new_with_options(url, &options)?);
        JsFuture::from(transport.ready()).await?;
        Ok(Self { transport })
    }

    pub async fn open_bi(&self) -> Result<WebTransportBidirectionalStream> {
        let stream = SendWrapper::new(web_sys::WebTransportBidirectionalStream::from(
            JsFuture::from(self.transport.create_bidirectional_stream()).await?,
        ));
        Ok(WebTransportBidirectionalStream { stream })
    }
}

#[derive(Debug)]
pub struct WebTransportBidirectionalStream {
    stream: SendWrapper<web_sys::WebTransportBidirectionalStream>,
}

impl WebTransportBidirectionalStream {
    pub fn receive(&self) -> Result<WebTransportReceiveStream> {
        let stream = SendWrapper::new(self.stream.readable());
        let reader = SendWrapper::new(web_sys::ReadableStreamDefaultReader::new(&stream)?);
        Ok(WebTransportReceiveStream {
            _stream: stream,
            reader,
            future: None,
            remaining: Vec::new(),
        })
    }

    pub fn send(&self) -> Result<WebTransportSendStream> {
        let stream = SendWrapper::new(self.stream.writable());
        let writer = SendWrapper::new(web_sys::WritableStreamDefaultWriter::new(&stream)?);
        Ok(WebTransportSendStream {
            _stream: stream,
            writer,
            ready: None,
            close: None,
            send: Vec::new(),
        })
    }
}

#[derive(Debug)]
pub struct WebTransportReceiveStream {
    _stream: SendWrapper<web_sys::WebTransportReceiveStream>,
    reader: SendWrapper<web_sys::ReadableStreamDefaultReader>,
    future: Option<SendWrapper<Pin<Box<JsFuture>>>>,
    remaining: Vec<u8>,
}

impl WebTransportReceiveStream {
    fn read_from_remaining(&mut self, buf: &mut ReadBuf<'_>) {
        let n = std::cmp::min(self.remaining.len(), buf.remaining());
        buf.put_slice(&self.remaining[..n]);
        self.remaining = self.remaining[n..].to_vec();
    }
}

impl AsyncRead for WebTransportReceiveStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = Pin::into_inner(self);

        if !this.remaining.is_empty() {
            this.read_from_remaining(buf);
            return Poll::Ready(Ok(()));
        }

        let future = this
            .future
            .get_or_insert_with(|| SendWrapper::new(Box::pin(JsFuture::from(this.reader.read()))));

        match ready!(future.as_mut().poll(cx)) {
            Ok(value) => {
                this.future = None;

                let done = Reflect::get(&value, &"done".into())
                    .map_err(|err| io::Error::new(io::ErrorKind::Other, Error::from(err)))?
                    .as_bool()
                    .ok_or_else(|| io::Error::new(io::ErrorKind::Other, "done is not a boolean"))?;
                if done {
                    return Poll::Ready(Ok(()));
                }

                let value = Uint8Array::from(
                    Reflect::get(&value, &"value".into())
                        .map_err(|err| io::Error::new(io::ErrorKind::Other, Error::from(err)))?,
                );
                this.remaining.resize(value.length() as usize, 0);
                value.copy_to(&mut this.remaining);

                this.read_from_remaining(buf);
                Poll::Ready(Ok(()))
            }
            Err(err) => {
                let err = Error::from(err);
                Poll::Ready(Err(io::Error::new(io::ErrorKind::Other, err)))
            }
        }
    }
}

#[derive(Debug)]
pub struct WebTransportSendStream {
    _stream: SendWrapper<web_sys::WebTransportSendStream>,
    writer: SendWrapper<web_sys::WritableStreamDefaultWriter>,
    ready: Option<SendWrapper<Pin<Box<JsFuture>>>>,
    close: Option<SendWrapper<Pin<Box<JsFuture>>>>,
    send: Vec<SendWrapper<Pin<Box<JsFuture>>>>,
}

impl WebTransportSendStream {
    fn poll_send(&mut self, cx: &mut Context<'_>) -> Poll<()> {
        self.send.retain_mut(|future| future.as_mut().poll(cx).is_pending());

        match self.send.is_empty() {
            true => Poll::Ready(()),
            false => Poll::Pending,
        }
    }
}

impl AsyncWrite for WebTransportSendStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = Pin::into_inner(self);
        let _ = this.poll_send(cx);

        let ready = this
            .ready
            .get_or_insert_with(|| SendWrapper::new(Box::pin(JsFuture::from(this.writer.ready()))));

        match ready!(ready.as_mut().poll(cx)) {
            Ok(_) => {
                this.ready = None;

                this.send.push(SendWrapper::new(Box::pin(JsFuture::from(
                    this.writer.write_with_chunk(&Uint8Array::from(buf)),
                ))));

                Poll::Ready(Ok(buf.len()))
            }
            Err(err) => {
                let err = Error::from(err);
                Poll::Ready(Err(io::Error::new(io::ErrorKind::Other, err)))
            }
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = Pin::into_inner(self);
        this.poll_send(cx).map(Ok)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = Pin::into_inner(self);
        ready!(this.poll_send(cx));

        let close = this
            .close
            .get_or_insert_with(|| SendWrapper::new(Box::pin(JsFuture::from(this.writer.close()))));

        match ready!(close.as_mut().poll(cx)) {
            Ok(_) => Poll::Ready(Ok(())),
            Err(err) => {
                let err = Error::from(err);
                Poll::Ready(Err(io::Error::new(io::ErrorKind::Other, err)))
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct Error(String);

impl From<JsValue> for Error {
    fn from(js_value: JsValue) -> Self {
        Self(js_value.as_string().unwrap_or_else(|| format!("{:?}", js_value)))
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for Error {}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_send_sync<T: Send + Sync>() {}

    #[allow(dead_code)]
    fn test_send_sync() {
        assert_send_sync::<WebTransport>();
        assert_send_sync::<WebTransportBidirectionalStream>();
        assert_send_sync::<WebTransportReceiveStream>();
        assert_send_sync::<WebTransportSendStream>();
    }
}
