use super::InternEvent;
use crate::{
    channel::Channels,
    connection::ConnectionState,
    double_key_map::DoubleKeyMap,
    handle::{ConnectionIdx, TransportIdx},
    protocol::{self, InternalC2S, InternalS2C},
    transport::{ServerTransport, TransportError},
    ChannelConfig, ChannelId, ConnectionHandle, NetworkEncode, NetworkError,
};
use crossbeam_channel::{Receiver, Sender};
use std::{
    sync::{
        atomic::{AtomicBool, AtomicU32, Ordering},
        Arc, RwLock,
    },
    thread,
};
use uuid::Uuid;

pub(super) fn start<T: ServerTransport + Send + Sync + 'static>(
    transport: T,
    transport_idx: TransportIdx,
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
        transport_idx,
        connections: RwLock::new(DoubleKeyMap::new()),
        next_connection_idx: AtomicU32::new(0),
        intern_events,
        channels,
        tasks_recv,
    });

    RunnerHandle { is_alive, sender: tasks_send }
}

pub struct RunnerHandle {
    is_alive: Arc<AtomicBool>,
    sender: Sender<RunnerTask>,
}

impl RunnerHandle {
    pub fn send_to(
        &self,
        message: Box<dyn NetworkEncode + Send>,
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
        message: Box<dyn NetworkEncode + Send>,
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

    intern_events: Sender<InternEvent>,
    channels: Arc<Channels>,

    tasks_recv: Receiver<RunnerTask>,
}

impl<T: ServerTransport + Send + Sync + 'static> Runner<T> {
    fn run(self) {
        let this = Arc::new(self);

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
        let (buf, id) = match self.transport.recv_from(buf) {
            Ok((size, id)) => (&buf[..size], id),
            Err(err) => match err {
                TransportError::Disconnected => {
                    self.disconnect_self_and_stop();
                    return;
                }
                TransportError::RemoteDisconnected(id) => {
                    if let Some((connection_idx, _, _)) =
                        self.connections.write().unwrap().remove2(&id)
                    {
                        self.transport.cleanup(id);
                        self.intern_events
                            .send(InternEvent::Disconnected(ConnectionHandle {
                                transport_idx: self.transport_idx,
                                connection_idx,
                            }))
                            .ok();
                    }
                    return;
                }
                TransportError::TimedOut => return,
                TransportError::Internal(err) => {
                    self.intern_events
                        .send(InternEvent::Error(None, NetworkError::TransportReceive(err)))
                        .ok();
                    return;
                }
            },
        };

        if buf.is_empty() {
            return; // Ignore empty message
        }

        let connections_lock = self.connections.read().unwrap();
        match connections_lock.get2(&id) {
            Some((connection_idx, _, (_, connection))) => {
                match ChannelId::try_new(buf[0]) {
                    Some(channel_id) => {
                        let message = &buf[1..];
                        match connection.decode_recv(channel_id, message) {
                            Some(Ok(())) => (),
                            Some(Err(err)) => {
                                self.intern_events
                                    .send(InternEvent::Error(
                                        Some(ConnectionHandle {
                                            transport_idx: self.transport_idx,
                                            connection_idx: *connection_idx,
                                        }),
                                        NetworkError::Decode(err),
                                    ))
                                    .ok();
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
                                    self.connections.write().unwrap().remove2(&id)
                                {
                                    self.transport.cleanup(id);
                                    self.intern_events
                                        .send(InternEvent::Disconnected(ConnectionHandle {
                                            transport_idx: self.transport_idx,
                                            connection_idx,
                                        }))
                                        .ok();
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
                        let connection =
                            Arc::new(ConnectionState::new(T::TRANSPORT_PROPERTIES, &self.channels));
                        let uuid = Uuid::new_v4();
                        self.connections.write().unwrap().insert(
                            connection_idx,
                            id,
                            (uuid, Arc::clone(&connection)),
                        );

                        // Send back connect message to client
                        // TODO: Send message reliably
                        let _ = self
                            .transport
                            .send_to(&InternalS2C::Connect(connection_idx, uuid).encode(), id);

                        // Send connected event
                        self.intern_events
                            .send(InternEvent::Connected(
                                ConnectionHandle {
                                    transport_idx: self.transport_idx,
                                    connection_idx,
                                },
                                connection,
                            ))
                            .ok();
                    }
                    Some(InternalC2S::Disconnect) => (), // Ignore
                    Some(InternalC2S::ProvideId(client_connection_idx, client_uuid)) => {
                        if let Some((connection_idx, current_id, (uuid, connection))) =
                            self.connections.read().unwrap().get1(&client_connection_idx)
                        {
                            if *current_id != id && *uuid == client_uuid {
                                // Client provided the correct connection_idx/uuid: update the connection id
                                self.connections.write().unwrap().insert(
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
                        let _ = self.transport.send_to(&InternalS2C::RequestId.encode(), id);
                        // TODO: Only send above once every 500ms or so
                    }
                }
            }
        }
    }

    fn tick_tasks(&self) {
        match self.tasks_recv.recv() {
            Ok(RunnerTask::SendTo { message, connection_idx, channel_id, channel_config }) => {
                let connections_lock = self.connections.read().unwrap();
                if let Some((_, id, _)) = connections_lock.get1(&connection_idx) {
                    match message.encode() {
                        Ok(payload) => {
                            if payload.len() > protocol::max_payload_size::<T>() {
                                // TODO: Split payload into multiple packets instead of disconnecting
                                self.disconnect_self_and_stop();
                                return;
                            }

                            //
                            let mut buf = Vec::with_capacity(payload.len() + 1);
                            buf.push(channel_id.value());
                            buf.extend_from_slice(&payload);

                            //
                            match self.transport.send_to(&buf, *id) {
                                Ok(()) => (),
                                Err(err) => {
                                    match err {
                                        TransportError::Disconnected => {
                                            self.disconnect_self_and_stop();
                                        }
                                        TransportError::RemoteDisconnected(()) => {
                                            drop(connections_lock);

                                            if let Some((connection_idx, id, _)) = self
                                                .connections
                                                .write()
                                                .unwrap()
                                                .remove1(&connection_idx)
                                            {
                                                self.transport.cleanup(id);
                                                self.intern_events
                                                    .send(InternEvent::Disconnected(
                                                        ConnectionHandle {
                                                            transport_idx: self.transport_idx,
                                                            connection_idx,
                                                        },
                                                    ))
                                                    .ok();
                                            }
                                        }
                                        TransportError::TimedOut => {
                                            if channel_config.reliable {
                                                // TODO: Retry instead of disconnecting
                                                self.disconnect_self_and_stop();
                                            }
                                        }
                                        TransportError::Internal(err) => {
                                            self.intern_events
                                                .send(InternEvent::Error(
                                                    Some(ConnectionHandle {
                                                        transport_idx: self.transport_idx,
                                                        connection_idx,
                                                    }),
                                                    NetworkError::TransportSend(err),
                                                ))
                                                .ok();

                                            if channel_config.reliable {
                                                // TODO: Retry instead of disconnecting
                                                self.disconnect_self_and_stop();
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        Err(_err) => {
                            // Encoding should never fail, disconnect
                            // TODO: Provide encode error to user
                            // TODO: Only disconnect from this connection, not the whole server
                            self.disconnect_self_and_stop();
                        }
                    }
                }
            }
            Ok(RunnerTask::Disconnect { connection_idx, notify_client }) => {
                if let Some((_, id, _)) = self.connections.write().unwrap().remove1(&connection_idx)
                {
                    if notify_client {
                        // Send disconnect message to client
                        let _ = self.transport.send_to(&InternalS2C::Disconnect.encode(), id);
                    }

                    // Cleanup connection
                    self.transport.cleanup(id);
                }
            }
            Err(_) => {
                // runner handle was dropped
                self.is_alive.store(false, Ordering::Relaxed);
            }
        }
    }

    fn disconnect_self_and_stop(&self) {
        self.intern_events.send(InternEvent::DisconnectedSelf).ok();
        self.is_alive.store(false, Ordering::Relaxed);
    }
}
