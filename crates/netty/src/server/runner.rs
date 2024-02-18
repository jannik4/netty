use super::{InternEvent, InternEventSender};
use crate::{
    channel::Channels,
    connection::ConnectionState,
    handle::{ConnectionIdx, TransportIdx},
    new_data::NewDataAvailable,
    protocol::{self, InternalC2S, InternalS2C},
    transport::{AsyncTransport, ServerTransport, TransportError},
    ChannelConfig, ChannelId, ConnectionHandle, NetworkEncode, NetworkError, Runtime, RuntimeExt,
};
use double_key_map::DoubleKeyMap;
use std::sync::{
    atomic::{AtomicBool, AtomicU32, Ordering},
    Arc,
};
use tokio::sync::{
    mpsc::{self, UnboundedReceiver, UnboundedSender},
    Mutex, RwLock,
};
use uuid::Uuid;

pub(super) fn start<T>(
    transport: AsyncTransport<T>,
    runtime: Arc<dyn Runtime>,
    transport_idx: TransportIdx,
    channels: Arc<Channels>,
    intern_events: InternEventSender,
    new_data: Arc<NewDataAvailable>,
) -> RunnerHandle
where
    T: ServerTransport + Send + Sync + 'static,
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
                        transport_idx,
                        connections: RwLock::new(DoubleKeyMap::new()),
                        next_connection_idx: AtomicU32::new(0),
                        intern_events,
                        channels,
                        tasks_recv: Mutex::new(tasks_recv),
                        new_data,
                        runtime,
                    });
                }
                Err(err) => {
                    intern_events.send(InternEvent::DisconnectedSelf(Some(err)));
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
    pub fn send_to(
        &self,
        message: Arc<dyn NetworkEncode + Send + Sync>,
        connection_idx: ConnectionIdx,
        channel_id: ChannelId,
        channel_config: ChannelConfig,
    ) {
        let _ = self.sender.send(RunnerTask::SendTo {
            message,
            connection_idx,
            channel_id,
            channel_config,
        });
    }

    pub fn disconnect(&self, connection_idx: ConnectionIdx, notify_client: bool) {
        let _ = self.sender.send(RunnerTask::Disconnect { connection_idx, notify_client });
    }
}

impl Drop for RunnerHandle {
    fn drop(&mut self) {
        self.is_alive.store(false, Ordering::Relaxed);
    }
}

enum RunnerTask {
    SendTo {
        message: Arc<dyn NetworkEncode + Send + Sync>,
        connection_idx: ConnectionIdx,
        channel_id: ChannelId,
        channel_config: ChannelConfig,
    },
    Disconnect {
        connection_idx: ConnectionIdx,
        notify_client: bool,
    },
}

struct Runner<T: ServerTransport> {
    is_alive: Arc<AtomicBool>,

    transport: T,
    transport_idx: TransportIdx,
    connections: RwLock<DoubleKeyMap<ConnectionIdx, T::ConnectionId, (Uuid, Arc<ConnectionState>)>>,

    next_connection_idx: AtomicU32,

    intern_events: InternEventSender,
    channels: Arc<Channels>,

    tasks_recv: Mutex<UnboundedReceiver<RunnerTask>>,

    new_data: Arc<NewDataAvailable>,

    runtime: Arc<dyn Runtime>,
}

impl<T: ServerTransport + Send + Sync + 'static> Runner<T> {
    fn run(self) {
        let this = Arc::new(self);

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
        let (buf, id) = match self.transport.recv_from(buf).await {
            Ok((size, id)) => (&buf[..size], id),
            Err(err) => match err {
                TransportError::Disconnected => {
                    self.disconnect_self_and_stop(None); // TODO: error = Some("transport disconnected ...")
                    return;
                }
                TransportError::RemoteDisconnected(id) => {
                    if let Some((connection_idx, _, _)) =
                        self.connections.write().await.remove2(&id)
                    {
                        self.transport.cleanup(id).await;
                        self.intern_events.send(InternEvent::Disconnected(ConnectionHandle {
                            transport_idx: self.transport_idx,
                            connection_idx,
                        }));
                    }
                    return;
                }
                TransportError::TimedOut => return,
                TransportError::Internal(err) => {
                    self.intern_events
                        .send(InternEvent::Error(None, NetworkError::TransportReceive(err)));
                    return;
                }
            },
        };

        if buf.is_empty() {
            return; // Ignore empty message
        }

        let connections_lock = self.connections.read().await;
        match connections_lock.get2(&id) {
            Some((connection_idx, _, (_, connection))) => {
                match ChannelId::try_new(buf[0]) {
                    Some(channel_id) => {
                        let message = &buf[1..];
                        match connection.decode_recv(channel_id, message) {
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
                                    self.transport.cleanup(id).await;
                                    self.intern_events.send(InternEvent::Disconnected(
                                        ConnectionHandle {
                                            transport_idx: self.transport_idx,
                                            connection_idx: *connection_idx,
                                        },
                                    ));
                                } else {
                                    self.intern_events.send(InternEvent::Error(
                                        Some(ConnectionHandle {
                                            transport_idx: self.transport_idx,
                                            connection_idx: *connection_idx,
                                        }),
                                        NetworkError::Decode(err),
                                    ));
                                }
                            }
                            None => {
                                // Ignore message on invalid channel
                                // TODO: log warning ???
                            }
                        }
                    }
                    None => {
                        drop(connections_lock);
                        match InternalC2S::decode(buf) {
                            Some(InternalC2S::Connect) => (), // Ignore
                            Some(InternalC2S::Disconnect) => {
                                if let Some((connection_idx, _, _)) =
                                    self.connections.write().await.remove2(&id)
                                {
                                    self.transport.cleanup(id).await;
                                    self.intern_events.send(InternEvent::Disconnected(
                                        ConnectionHandle {
                                            transport_idx: self.transport_idx,
                                            connection_idx,
                                        },
                                    ));
                                }
                            }
                            Some(InternalC2S::ProvideId(_, _)) => (), // Ignore
                            None => (),                               // Ignore
                        }
                    }
                }
            }
            None => {
                drop(connections_lock);

                match InternalC2S::decode(buf) {
                    Some(InternalC2S::Connect) => {
                        // Create a new connection
                        let connection_idx =
                            ConnectionIdx(self.next_connection_idx.fetch_add(1, Ordering::Relaxed));
                        let connection = Arc::new(ConnectionState::new(
                            T::TRANSPORT_PROPERTIES,
                            Arc::clone(&self.new_data),
                            &self.channels,
                        ));
                        let uuid = Uuid::new_v4();
                        self.connections.write().await.insert(
                            connection_idx,
                            id,
                            (uuid, Arc::clone(&connection)),
                        );

                        // Send back connect message to client
                        // TODO: Send message reliably
                        let _ = self
                            .transport
                            .send_to(&InternalS2C::Connect(connection_idx, uuid).encode(), id)
                            .await;

                        // Send connected event
                        self.intern_events.send(InternEvent::IncomingConnection(
                            ConnectionHandle { transport_idx: self.transport_idx, connection_idx },
                            connection,
                        ));
                    }
                    Some(InternalC2S::Disconnect) => (), // Ignore
                    Some(InternalC2S::ProvideId(client_connection_idx, client_uuid)) => {
                        if let Some((connection_idx, current_id, (uuid, connection))) =
                            self.connections.read().await.get1(&client_connection_idx)
                        {
                            if *current_id != id && *uuid == client_uuid {
                                // Client provided the correct connection_idx/uuid: update the connection id
                                self.connections.write().await.insert(
                                    *connection_idx,
                                    id,
                                    (*uuid, Arc::clone(connection)),
                                );
                            }
                        }
                    }
                    None => {
                        // This might be a message from a connected client, which has a new connection id.
                        // Request the new connection id from the client.
                        let _ = self.transport.send_to(&InternalS2C::RequestId.encode(), id).await;
                        // TODO: Only send above once every 500ms or so
                    }
                }
            }
        }
    }

    async fn tick_tasks(&self) {
        match self.tasks_recv.lock().await.recv().await {
            Some(RunnerTask::SendTo { message, connection_idx, channel_id, channel_config }) => {
                let connections_lock = self.connections.read().await;
                if let Some((_, id, _)) = connections_lock.get1(&connection_idx) {
                    match message.encode() {
                        Ok(payload) => {
                            if payload.len() > protocol::max_payload_size::<T>() {
                                // TODO: Split payload into multiple packets instead of disconnecting
                                self.disconnect_self_and_stop(None);
                                return;
                            }

                            //
                            let mut buf = Vec::with_capacity(payload.len() + 1);
                            buf.push(channel_id.value());
                            buf.extend_from_slice(&payload);

                            //
                            match self.transport.send_to(&buf, *id).await {
                                Ok(()) => (),
                                Err(err) => {
                                    match err {
                                        TransportError::Disconnected => {
                                            self.disconnect_self_and_stop(None);
                                        }
                                        TransportError::RemoteDisconnected(()) => {
                                            drop(connections_lock);

                                            if let Some((connection_idx, id, _)) = self
                                                .connections
                                                .write()
                                                .await
                                                .remove1(&connection_idx)
                                            {
                                                self.transport.cleanup(id).await;
                                                self.intern_events.send(InternEvent::Disconnected(
                                                    ConnectionHandle {
                                                        transport_idx: self.transport_idx,
                                                        connection_idx,
                                                    },
                                                ));
                                            }
                                        }
                                        TransportError::TimedOut => {
                                            if channel_config.reliable {
                                                // TODO: Retry instead of disconnecting
                                                self.disconnect_self_and_stop(None);
                                            }
                                        }
                                        TransportError::Internal(err) => {
                                            self.intern_events.send(InternEvent::Error(
                                                Some(ConnectionHandle {
                                                    transport_idx: self.transport_idx,
                                                    connection_idx,
                                                }),
                                                NetworkError::TransportSend(err),
                                            ));

                                            if channel_config.reliable {
                                                // TODO: Retry instead of disconnecting
                                                self.disconnect_self_and_stop(None);
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        Err(err) => {
                            // Encoding should never fail, disconnect
                            // TODO: Only disconnect from this connection, not the whole server
                            self.disconnect_self_and_stop(Some(err.into()));
                        }
                    }
                }
            }
            Some(RunnerTask::Disconnect { connection_idx, notify_client }) => {
                if let Some((_, id, _)) = self.connections.write().await.remove1(&connection_idx) {
                    if notify_client {
                        // Send disconnect message to client
                        let _ = self.transport.send_to(&InternalS2C::Disconnect.encode(), id).await;
                    }

                    // Cleanup connection
                    self.transport.cleanup(id).await;
                }
            }
            None => {
                // runner handle was dropped
                self.is_alive.store(false, Ordering::Relaxed);
            }
        }
    }

    fn disconnect_self_and_stop(&self, error: Option<NetworkError>) {
        self.intern_events.send(InternEvent::DisconnectedSelf(error));
        self.is_alive.store(false, Ordering::Relaxed);
    }
}
