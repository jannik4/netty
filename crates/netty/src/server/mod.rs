mod runner;

use self::runner::RunnerHandle;
use crate::{
    channel::Channels, connection::ConnectionState, handle::TransportIdx,
    transport::ServerTransport, ConnectionHandle, NetworkDecode, NetworkEncode, NetworkError,
    NetworkMessage,
};
use crossbeam_channel::{Receiver, Sender};
use std::{collections::HashMap, sync::Arc};

enum ServerState {
    Disconnected,
    Running(ServerRunning),
}

struct ServerRunning {
    runners: Vec<RunnerHandle>,
    intern_recv: Receiver<InternEvent>,
    connections: HashMap<ConnectionHandle, Arc<ConnectionState>>,
}

impl ServerRunning {
    fn runner(&self, transport_idx: TransportIdx) -> &RunnerHandle {
        self.runners
            .get(transport_idx.0 as usize)
            .unwrap_or_else(|| panic!("transport {} does not exist", transport_idx.0))
    }
}

#[cfg_attr(feature = "bevy", derive(bevy_ecs::system::Resource))]
pub struct Server {
    channels: Arc<Channels>,
    state: ServerState,
}

impl Server {
    pub fn new(channels: Arc<Channels>) -> Self {
        Self { channels, state: ServerState::Disconnected }
    }

    pub fn start<T: ServerTransports>(&mut self, transports: T) {
        self.stop();

        let (intern_send, intern_recv) = crossbeam_channel::unbounded();
        let runners = transports.start(ServerTransportsParams {
            channels: Arc::clone(&self.channels),
            intern_events: intern_send,
        });

        self.state = ServerState::Running(ServerRunning {
            runners,
            intern_recv,
            connections: HashMap::new(),
        });
    }

    pub fn stop(&mut self) {
        if let ServerState::Running(running) = &self.state {
            for handle in running.connections.keys() {
                running.runner(handle.transport_idx).disconnect(handle.connection_idx, true);
            }
        }

        self.state = ServerState::Disconnected;
    }

    pub fn disconnect(&mut self, handle: ConnectionHandle) {
        if let ServerState::Running(running) = &mut self.state {
            running.runner(handle.transport_idx).disconnect(handle.connection_idx, true);
            running.connections.remove(&handle);
        }
    }

    pub fn process_events(&mut self) -> impl Iterator<Item = ServerEvent> + '_ {
        ProcessEvents(&mut self.state)
    }

    pub fn send_to<T: NetworkEncode + NetworkMessage + Send + Sync + 'static>(
        &self,
        message: T,
        handle: ConnectionHandle,
    ) {
        self.channels.assert_send_exists(T::CHANNEL_ID);
        if let ServerState::Running(running) = &self.state {
            running.runner(handle.transport_idx).send_to(
                Arc::new(message),
                handle.connection_idx,
                T::CHANNEL_ID,
                T::CHANNEL_CONFIG,
            );
        }
    }

    pub fn broadcast<T: NetworkEncode + NetworkMessage + Send + Sync + 'static>(&self, message: T) {
        self.channels.assert_send_exists(T::CHANNEL_ID);
        if let ServerState::Running(running) = &self.state {
            let message: Arc<dyn NetworkEncode + Send + Sync> = Arc::new(message);
            for handle in running.connections.keys() {
                running.runner(handle.transport_idx).send_to(
                    Arc::clone(&message),
                    handle.connection_idx,
                    T::CHANNEL_ID,
                    T::CHANNEL_CONFIG,
                );
            }
        }
    }

    pub fn recv<T: NetworkDecode + NetworkMessage + 'static>(
        &self,
    ) -> impl Iterator<Item = (T, ConnectionHandle)> + '_ {
        let running = match &self.state {
            ServerState::Running(running) => Some(running),
            ServerState::Disconnected => None,
        };

        running
            .into_iter()
            .flat_map(|running| running.connections.keys())
            .flat_map(|handle| self.recv_from::<T>(*handle).map(|msg| (msg, *handle)))
    }

    pub fn recv_from<T: NetworkDecode + NetworkMessage + 'static>(
        &self,
        handle: ConnectionHandle,
    ) -> impl Iterator<Item = T> + '_ {
        match &self.state {
            ServerState::Running(running) => match running.connections.get(&handle) {
                Some(connection) => RecvIterator::Receiver(connection.get_receiver()),
                None => RecvIterator::NotConnected,
            },
            ServerState::Disconnected => RecvIterator::NotConnected,
        }
    }

    pub fn is_running(&self) -> bool {
        match self.state {
            ServerState::Disconnected => false,
            ServerState::Running { .. } => true,
        }
    }
}

#[derive(Debug)]
#[cfg_attr(feature = "bevy", derive(bevy_ecs::event::Event))]
pub enum ServerEvent {
    Connected(ConnectionHandle),
    Disconnected(ConnectionHandle),
    DisconnectedSelf,
    Error(Option<ConnectionHandle>, NetworkError),
}

enum InternEvent {
    Connected(ConnectionHandle, Arc<ConnectionState>),
    Disconnected(ConnectionHandle),
    DisconnectedSelf,
    Error(Option<ConnectionHandle>, NetworkError),
}

enum RecvIterator<'a, T> {
    NotConnected,
    Receiver(&'a Receiver<T>),
}

impl<T> Iterator for RecvIterator<'_, T> {
    type Item = T;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::NotConnected => None,
            Self::Receiver(recv) => recv.try_recv().ok(),
        }
    }
}

struct ProcessEvents<'a>(&'a mut ServerState);

impl Iterator for ProcessEvents<'_> {
    type Item = ServerEvent;

    fn next(&mut self) -> Option<Self::Item> {
        match self.0 {
            ServerState::Disconnected => None,
            ServerState::Running(running) => {
                let event = running.intern_recv.try_recv().ok()?;

                Some(loop {
                    match event {
                        InternEvent::Connected(handle, connection) => {
                            running.connections.insert(handle, connection);
                            break ServerEvent::Connected(handle);
                        }
                        InternEvent::Disconnected(handle) => {
                            if running.connections.remove(&handle).is_some() {
                                break ServerEvent::Disconnected(handle);
                            }
                        }
                        InternEvent::DisconnectedSelf => {
                            *self.0 = ServerState::Disconnected;
                            break ServerEvent::DisconnectedSelf;
                        }
                        InternEvent::Error(handle, error) => {
                            break ServerEvent::Error(handle, error);
                        }
                    }
                })
            }
        }
    }
}

pub struct ServerTransportsParams {
    channels: Arc<Channels>,
    intern_events: Sender<InternEvent>,
}

pub trait ServerTransports {
    fn start(self, params: ServerTransportsParams) -> Vec<RunnerHandle>;
}

impl<A> ServerTransports for A
where
    A: ServerTransport + Send + Sync + 'static,
{
    fn start(self, params: ServerTransportsParams) -> Vec<RunnerHandle> {
        vec![runner::start(self, TransportIdx(0), params.channels, params.intern_events)]
    }
}

impl<A, B> ServerTransports for (A, B)
where
    A: ServerTransport + Send + Sync + 'static,
    B: ServerTransport + Send + Sync + 'static,
{
    fn start(self, params: ServerTransportsParams) -> Vec<RunnerHandle> {
        let (a, b) = self;

        vec![
            runner::start(
                a,
                TransportIdx(0),
                Arc::clone(&params.channels),
                params.intern_events.clone(),
            ),
            runner::start(b, TransportIdx(1), params.channels, params.intern_events),
        ]
    }
}

// TODO: more tuples ...
