use super::{InternEvent, InternEventSender};
use crate::{
    connection::ConnectionState,
    new_data::NewDataAvailable,
    protocol::{self, InternalC2S, InternalS2C},
    transport::{AsyncTransport, ClientTransport, TransportError},
    ChannelConfig, ChannelId, Channels, NetworkEncode, NetworkError, Runtime, RuntimeExt,
};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::sync::{
    mpsc::{self, UnboundedReceiver, UnboundedSender},
    Mutex,
};

pub(super) fn start<T>(
    transport: AsyncTransport<T>,
    runtime: Arc<dyn Runtime>,
    channels: Arc<Channels>,
    intern_events: InternEventSender,
    new_data: Arc<NewDataAvailable>,
) -> RunnerHandle
where
    T: ClientTransport + Send + Sync + 'static,
{
    // TODO: Remove this when reliability and ordering are implemented
    assert!(T::IS_ORDERED);
    assert!(T::IS_RELIABLE);

    let is_alive = Arc::new(AtomicBool::new(true));
    let (tasks_send, tasks_recv) = mpsc::unbounded_channel();

    runtime.spawn({
        let is_alive = Arc::clone(&is_alive);
        let runtime = Arc::clone(&runtime);
        async move {
            match transport.start(Arc::clone(&runtime)).await {
                Ok(transport) => {
                    Runner::run(Runner {
                        is_alive,
                        transport,
                        connection: Arc::new(ConnectionState::new(
                            T::TRANSPORT_PROPERTIES,
                            new_data,
                            &channels,
                        )),
                        is_connected: AtomicBool::new(false),
                        intern_events,
                        channels,
                        tasks_recv: Mutex::new(tasks_recv),
                        runtime,
                    });
                }
                Err(err) => {
                    intern_events.send(InternEvent::Disconnected(Some(err)));
                    is_alive.store(false, Ordering::Relaxed);
                }
            }
        }
    });

    RunnerHandle { is_alive, sender: tasks_send }
}

pub struct RunnerHandle {
    is_alive: Arc<AtomicBool>,
    sender: UnboundedSender<RunnerTask>,
}

impl RunnerHandle {
    pub fn send(
        &self,
        message: Box<dyn NetworkEncode + Send>,
        channel_id: ChannelId,
        channel_config: ChannelConfig,
    ) {
        let _ = self.sender.send(RunnerTask::Send { message, channel_id, channel_config });
    }

    pub fn disconnect(&self, notify_server: bool) {
        let _ = self.sender.send(RunnerTask::Disconnect { notify_server });
    }
}

impl Drop for RunnerHandle {
    fn drop(&mut self) {
        self.is_alive.store(false, Ordering::Relaxed);
    }
}

enum RunnerTask {
    Send {
        message: Box<dyn NetworkEncode + Send>,
        channel_id: ChannelId,
        channel_config: ChannelConfig,
    },
    Disconnect {
        notify_server: bool,
    },
}

struct Runner<T> {
    is_alive: Arc<AtomicBool>,

    transport: T,
    connection: Arc<ConnectionState>,
    is_connected: AtomicBool,

    intern_events: InternEventSender,
    channels: Arc<Channels>,

    tasks_recv: Mutex<UnboundedReceiver<RunnerTask>>,

    runtime: Arc<dyn Runtime>,
}

impl<T: ClientTransport + Send + Sync + 'static> Runner<T> {
    fn run(self) {
        let this = Arc::new(self);

        // Send connect message to server
        this.runtime.spawn({
            let this = Arc::clone(&this);
            async move {
                while this.is_alive.load(Ordering::Relaxed)
                    && !this.is_connected.load(Ordering::Relaxed)
                {
                    let _ = this.transport.send(&InternalC2S::Connect.encode()).await;
                    this.runtime.sleep(Duration::from_millis(500)).await;
                }
            }
        });

        // Tick recv
        this.runtime.spawn({
            let this = Arc::clone(&this);
            async move {
                let mut buf = vec![0; T::MAX_PACKET_SIZE.get()]; // TODO: array or vec?
                while this.is_alive.load(Ordering::Relaxed) {
                    this.tick_recv(&mut buf).await;
                }
            }
        });

        // Tick tasks
        this.runtime.spawn({
            let this = Arc::clone(&this);
            async move {
                while this.is_alive.load(Ordering::Relaxed) {
                    this.tick_tasks().await;
                }
            }
        });
    }

    async fn tick_recv(&self, buf: &mut [u8]) {
        let buf = match self.transport.recv(buf).await {
            Ok(size) => &buf[..size],
            Err(err) => match err {
                TransportError::Disconnected => {
                    self.disconnect_and_stop(false, None).await; // TODO: error = Some("transport disconnected ...")
                    return;
                }
                TransportError::RemoteDisconnected(never) => match never {},
                TransportError::TimedOut => return,
                TransportError::Internal(err) => {
                    self.intern_events
                        .send(InternEvent::Error(NetworkError::TransportReceive(err)));
                    return;
                }
            },
        };

        if buf.is_empty() {
            return; // Ignore empty message
        }

        if self.is_connected.load(Ordering::Relaxed) {
            match ChannelId::try_new(buf[0]) {
                Some(channel_id) => {
                    let message = &buf[1..];
                    match self.connection.decode_recv(channel_id, message) {
                        Some(Ok(())) => (),
                        Some(Err(err)) => {
                            // If the transport layer is reliable and the channel is reliable, disconnect
                            // since the message is lost permanently
                            if T::IS_RELIABLE
                                && self
                                    .channels
                                    .recv
                                    .get(&channel_id)
                                    .map_or(false, |channel| channel.0.config.reliable)
                            {
                                self.disconnect_and_stop(false, Some(err.into())).await;
                            } else {
                                self.intern_events
                                    .send(InternEvent::Error(NetworkError::Decode(err)));
                            }
                        }
                        None => {
                            // Ignore message on invalid channel
                            // TODO: log warning ???
                        }
                    }
                }
                None => {
                    match InternalS2C::decode(buf) {
                        Some(InternalS2C::Connect(_, _)) => (), // Ignore
                        Some(InternalS2C::Disconnect) => {
                            self.disconnect_and_stop(false, None).await;
                        }
                        Some(InternalS2C::RequestId) => {
                            // TODO: Send server [ProvideId + connection_idx + uuid]
                            // TODO: Only send above once every 500ms or so
                        }
                        None => (), // Ignore
                    }
                }
            }
        } else {
            match InternalS2C::decode(buf) {
                Some(InternalS2C::Connect(_connection_idx, _uuid)) => {
                    // TODO: Store connection_idx and uuid for ProvideId

                    self.is_connected.store(true, Ordering::Relaxed);
                    self.intern_events.send(InternEvent::Connected(Arc::clone(&self.connection)));
                }
                Some(InternalS2C::Disconnect) => {
                    self.disconnect_and_stop(false, None).await;
                }
                Some(InternalS2C::RequestId) => (), // Ignore
                None => (),                         // Ignore
            }
        }
    }

    async fn tick_tasks(&self) {
        match self.tasks_recv.lock().await.recv().await {
            Some(RunnerTask::Send { message, channel_id, channel_config }) => {
                if self.is_connected.load(Ordering::Relaxed) {
                    match message.encode() {
                        Ok(payload) => {
                            if payload.len() > protocol::max_payload_size::<T>() {
                                // TODO: Split payload into multiple packets instead of disconnecting
                                self.disconnect_and_stop(false, None).await;
                                return;
                            }

                            //
                            let mut buf = Vec::with_capacity(payload.len() + 1);
                            buf.push(channel_id.value());
                            buf.extend_from_slice(&payload);

                            //
                            match self.transport.send(&buf).await {
                                Ok(()) => (),
                                Err(err) => {
                                    match err {
                                        TransportError::Disconnected => {
                                            self.disconnect_and_stop(false, None).await;
                                        }
                                        TransportError::RemoteDisconnected(never) => match never {},
                                        TransportError::TimedOut => {
                                            if channel_config.reliable {
                                                // TODO: Retry instead of disconnecting
                                                self.disconnect_and_stop(false, None).await;
                                            }
                                        }
                                        TransportError::Internal(err) => {
                                            self.intern_events.send(InternEvent::Error(
                                                NetworkError::TransportSend(err),
                                            ));

                                            if channel_config.reliable {
                                                // TODO: Retry instead of disconnecting
                                                self.disconnect_and_stop(false, None).await;
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        Err(err) => {
                            // Encoding should never fail, disconnect
                            self.disconnect_and_stop(true, Some(err.into())).await;
                        }
                    }
                } else {
                    // TODO: Store reliable messages and send them when connected instead of disconnecting
                    self.disconnect_and_stop(false, None).await;
                }
            }
            Some(RunnerTask::Disconnect { notify_server }) => {
                self.disconnect_and_stop(notify_server, None).await;
            }
            None => {
                // runner handle was dropped
                self.is_alive.store(false, Ordering::Relaxed);
            }
        }
    }

    async fn disconnect_and_stop(&self, notify_server: bool, error: Option<NetworkError>) {
        if notify_server {
            // Send disconnect message to server
            let _ = self.transport.send(&InternalC2S::Disconnect.encode()).await;
        }

        self.intern_events.send(InternEvent::Disconnected(error));
        self.is_alive.store(false, Ordering::Relaxed);
    }
}
