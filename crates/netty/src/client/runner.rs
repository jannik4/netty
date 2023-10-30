use super::InternEvent;
use crate::{
    connection::ConnectionState,
    protocol::{self, InternalC2S, InternalS2C},
    transport::{ClientTransport, TransportError},
    ChannelConfig, ChannelId, Channels, NetworkEncode, NetworkError,
};
use crossbeam_channel::{Receiver, Sender};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread,
    time::Duration,
};

pub(super) fn start<T: ClientTransport + Send + Sync + 'static>(
    transport: T,
    channels: Arc<Channels>,
    intern_events: Sender<InternEvent>,
) -> RunnerHandle {
    // TODO: Remove this when reliability and ordering are implemented
    assert!(T::IS_ORDERED);
    assert!(T::IS_RELIABLE);

    let is_alive = Arc::new(AtomicBool::new(true));
    let (tasks_send, tasks_recv) = crossbeam_channel::unbounded();

    Runner::run(Runner {
        is_alive: Arc::clone(&is_alive),
        transport,
        connection: Arc::new(ConnectionState::new(T::TRANSPORT_PROPERTIES, &channels)),
        is_connected: AtomicBool::new(false),
        intern_events,
        tasks_recv,
    });

    RunnerHandle { is_alive, sender: tasks_send }
}

pub struct RunnerHandle {
    is_alive: Arc<AtomicBool>,
    sender: Sender<RunnerTask>,
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

    intern_events: Sender<InternEvent>,

    tasks_recv: Receiver<RunnerTask>,
}

impl<T: ClientTransport + Send + Sync + 'static> Runner<T> {
    fn run(self) {
        let this = Arc::new(self);

        // Send connect message to server
        thread::spawn({
            let this = Arc::clone(&this);
            move || {
                while this.is_alive.load(Ordering::Relaxed)
                    && !this.is_connected.load(Ordering::Relaxed)
                {
                    let _ = this.transport.send(&InternalC2S::Connect.encode());
                    thread::sleep(Duration::from_millis(500));
                }
            }
        });

        // Tick recv
        thread::spawn({
            let this = Arc::clone(&this);
            move || {
                let mut buf = vec![0; T::MAX_PACKET_SIZE.get()]; // TODO: array or vec?
                while this.is_alive.load(Ordering::Relaxed) {
                    this.tick_recv(&mut buf);
                }
            }
        });

        // Tick tasks
        thread::spawn({
            let this = Arc::clone(&this);
            move || {
                while this.is_alive.load(Ordering::Relaxed) {
                    this.tick_tasks();
                }
            }
        });
    }

    fn tick_recv(&self, buf: &mut [u8]) {
        let buf = match self.transport.recv(buf) {
            Ok(size) => &buf[..size],
            Err(err) => match err {
                TransportError::Disconnected => {
                    self.disconnect_and_stop(false);
                    return;
                }
                TransportError::RemoteDisconnected(never) => match never {},
                TransportError::TimedOut => return,
                TransportError::Internal(err) => {
                    self.intern_events
                        .send(InternEvent::Error(NetworkError::TransportReceive(err)))
                        .ok();
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
                            self.intern_events
                                .send(InternEvent::Error(NetworkError::Decode(err)))
                                .ok();
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
                            self.disconnect_and_stop(false);
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
                    self.intern_events
                        .send(InternEvent::Connected(Arc::clone(&self.connection)))
                        .ok();
                }
                Some(InternalS2C::Disconnect) => {
                    self.disconnect_and_stop(false);
                }
                Some(InternalS2C::RequestId) => (), // Ignore
                None => (),                         // Ignore
            }
        }
    }

    fn tick_tasks(&self) {
        match self.tasks_recv.recv() {
            Ok(RunnerTask::Send { message, channel_id, channel_config }) => {
                if self.is_connected.load(Ordering::Relaxed) {
                    match message.encode() {
                        Ok(payload) => {
                            if payload.len() > protocol::max_payload_size::<T>() {
                                // TODO: Split payload into multiple packets instead of disconnecting
                                self.disconnect_and_stop(false);
                                return;
                            }

                            //
                            let mut buf = Vec::with_capacity(payload.len() + 1);
                            buf.push(channel_id.value());
                            buf.extend_from_slice(&payload);

                            //
                            match self.transport.send(&buf) {
                                Ok(()) => (),
                                Err(err) => {
                                    match err {
                                        TransportError::Disconnected => {
                                            self.disconnect_and_stop(false);
                                        }
                                        TransportError::RemoteDisconnected(never) => match never {},
                                        TransportError::TimedOut => {
                                            if channel_config.reliable {
                                                // TODO: Retry instead of disconnecting
                                                self.disconnect_and_stop(false);
                                            }
                                        }
                                        TransportError::Internal(err) => {
                                            self.intern_events
                                                .send(InternEvent::Error(
                                                    NetworkError::TransportSend(err),
                                                ))
                                                .ok();

                                            if channel_config.reliable {
                                                // TODO: Retry instead of disconnecting
                                                self.disconnect_and_stop(false);
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        Err(_err) => {
                            // Encoding should never fail, disconnect
                            // TODO: Provide encode error to user
                            self.disconnect_and_stop(true);
                        }
                    }
                } else {
                    // TODO: Store reliable messages and send them when connected instead of disconnecting
                    self.disconnect_and_stop(false);
                }
            }
            Ok(RunnerTask::Disconnect { notify_server }) => self.disconnect_and_stop(notify_server),
            Err(_) => {
                // runner handle was dropped
                self.is_alive.store(false, Ordering::Relaxed);
            }
        }
    }

    fn disconnect_and_stop(&self, notify_server: bool) {
        if notify_server {
            // Send disconnect message to server
            let _ = self.transport.send(&InternalC2S::Disconnect.encode());
        }

        self.intern_events.send(InternEvent::Disconnected).ok();
        self.is_alive.store(false, Ordering::Relaxed);
    }
}
