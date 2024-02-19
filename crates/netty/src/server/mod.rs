mod runner;

use self::runner::RunnerHandle;
use crate::{
    channel::Channels,
    connection::ConnectionState,
    handle::TransportIdx,
    new_data::NewDataAvailable,
    transport::{AsyncTransport, ServerTransport},
    ConnectionHandle, NetworkDecode, NetworkEncode, NetworkError, NetworkMessage, Runtime,
};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, Weak},
    time::Duration,
};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

enum ServerState {
    Disconnected,
    Running(ServerRunning),
}

struct ServerRunning {
    runners: Vec<(bool, Arc<RunnerHandle>)>,
    intern_send: InternEventSender,
    intern_recv: UnboundedReceiver<InternEvent>,
    connections: HashMap<ConnectionHandle, Arc<ConnectionState>>,
    incoming: HashMap<ConnectionHandle, Arc<ConnectionState>>,
    new_data: Arc<NewDataAvailable>,
}

impl ServerRunning {
    fn runner(&self, transport_idx: TransportIdx) -> &Arc<RunnerHandle> {
        self.runners
            .get(transport_idx.0 as usize)
            .map(|(_, runner)| runner)
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

    pub fn start(&mut self, transports: impl AsyncServerTransports, runtime: Arc<dyn Runtime>) {
        self.stop();

        let (intern_send, intern_recv) = mpsc::unbounded_channel();
        let new_data = Arc::new(NewDataAvailable::new());
        let runners = Box::new(transports)
            .start(ServerTransportsParams {
                channels: Arc::clone(&self.channels),
                intern_events: InternEventSender {
                    intern_send: intern_send.clone(),
                    new_data: Arc::clone(&new_data),
                },
                new_data: Arc::clone(&new_data),
                runtime,
            })
            .into_iter()
            .map(|runner| (false, runner))
            .collect();

        self.state = ServerState::Running(ServerRunning {
            runners,
            intern_send: InternEventSender {
                intern_send: intern_send.clone(),
                new_data: Arc::clone(&new_data),
            },
            intern_recv,
            connections: HashMap::new(),
            incoming: HashMap::new(),
            new_data,
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

    pub fn broadcast_except<T: NetworkEncode + NetworkMessage + Send + Sync + 'static>(
        &self,
        message: T,
        except: ConnectionHandle,
    ) {
        self.channels.assert_send_exists(T::CHANNEL_ID);
        if let ServerState::Running(running) = &self.state {
            let message: Arc<dyn NetworkEncode + Send + Sync> = Arc::new(message);
            for handle in running.connections.keys() {
                if *handle != except {
                    running.runner(handle.transport_idx).send_to(
                        Arc::clone(&message),
                        handle.connection_idx,
                        T::CHANNEL_ID,
                        T::CHANNEL_CONFIG,
                    );
                }
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

    pub fn accept(&mut self, mut incoming: IncomingConnection) -> Option<ConnectionHandle> {
        let ServerState::Running(running) = &mut self.state else { return None };

        match running.incoming.remove(&incoming.handle) {
            Some(connection) => {
                incoming.disconnect_on_drop = None;
                running.connections.insert(incoming.handle, connection);

                Some(incoming.handle)
            }
            None => None,
        }
    }

    pub fn is_running(&self) -> bool {
        match self.state {
            ServerState::Disconnected => false,
            ServerState::Running(_) => true,
        }
    }

    pub fn wait_timeout(&self, duration: Duration) -> bool {
        match &self.state {
            ServerState::Disconnected => false,
            ServerState::Running(running) => running.new_data.wait_timeout(duration),
        }
    }
}

#[derive(Debug)]
#[cfg_attr(feature = "bevy", derive(bevy_ecs::event::Event))]
pub enum ServerEvent {
    TransportIsUp(TransportIdx),
    ServerIsUp,
    IncomingConnection(IncomingConnection),
    Disconnected(ConnectionHandle),
    DisconnectedSelf(Option<NetworkError>),
    Error(Option<ConnectionHandle>, NetworkError),
}

pub struct IncomingConnection {
    handle: ConnectionHandle,
    disconnect_on_drop: Option<(InternEventSender, Weak<RunnerHandle>)>,
}

impl Drop for IncomingConnection {
    fn drop(&mut self) {
        if let Some((intern, runner)) = &self.disconnect_on_drop {
            intern.send(InternEvent::Disconnected(self.handle));
            if let Some(runner) = runner.upgrade() {
                runner.disconnect(self.handle.connection_idx, true);
            }
        }
    }
}

impl std::fmt::Debug for IncomingConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IncomingConnection").field("handle", &self.handle).finish()
    }
}

enum InternEvent {
    TransportIsUp(TransportIdx),
    ServerIsUp,
    IncomingConnection(ConnectionHandle, Arc<ConnectionState>),
    Disconnected(ConnectionHandle),
    DisconnectedSelf(Option<NetworkError>),
    Error(Option<ConnectionHandle>, NetworkError),
}

#[derive(Clone)]
struct InternEventSender {
    intern_send: UnboundedSender<InternEvent>,
    new_data: Arc<NewDataAvailable>,
}

impl InternEventSender {
    fn send(&self, event: InternEvent) {
        self.intern_send.send(event).ok();
        self.new_data.notify();
    }
}

enum RecvIterator<'a, T> {
    NotConnected,
    Receiver(&'a Mutex<UnboundedReceiver<T>>),
}

impl<T> Iterator for RecvIterator<'_, T> {
    type Item = T;

    fn next(&mut self) -> Option<Self::Item> {
        match &*self {
            Self::NotConnected => None,
            Self::Receiver(recv) => recv.lock().unwrap().try_recv().ok(),
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
                        InternEvent::TransportIsUp(idx) => {
                            running.runners[idx.0 as usize].0 = true;

                            if running.runners.iter().all(|(up, _)| *up) {
                                running.intern_send.send(InternEvent::ServerIsUp);
                            }

                            break ServerEvent::TransportIsUp(idx);
                        }
                        InternEvent::ServerIsUp => break ServerEvent::ServerIsUp,
                        InternEvent::IncomingConnection(handle, connection) => {
                            running.incoming.insert(handle, connection);
                            break ServerEvent::IncomingConnection(IncomingConnection {
                                handle,
                                disconnect_on_drop: Some((
                                    running.intern_send.clone(),
                                    Arc::downgrade(running.runner(handle.transport_idx)),
                                )),
                            });
                        }
                        InternEvent::Disconnected(handle) => {
                            if running.connections.remove(&handle).is_some() {
                                break ServerEvent::Disconnected(handle);
                            } else {
                                running.incoming.remove(&handle);
                            }
                        }
                        InternEvent::DisconnectedSelf(error) => {
                            *self.0 = ServerState::Disconnected;
                            break ServerEvent::DisconnectedSelf(error);
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
    intern_events: InternEventSender,
    new_data: Arc<NewDataAvailable>,
    runtime: Arc<dyn Runtime>,
}

pub trait AsyncServerTransports {
    fn start(self: Box<Self>, params: ServerTransportsParams) -> Vec<Arc<RunnerHandle>>;
}

impl AsyncServerTransports for Box<dyn AsyncServerTransports + Send + Sync + 'static> {
    fn start(self: Box<Self>, params: ServerTransportsParams) -> Vec<Arc<RunnerHandle>> {
        (*self).start(params)
    }
}

impl<A> AsyncServerTransports for AsyncTransport<A>
where
    A: ServerTransport + Send + Sync + 'static,
{
    fn start(self: Box<Self>, params: ServerTransportsParams) -> Vec<Arc<RunnerHandle>> {
        let a = *self;
        vec![Arc::new(runner::start(
            a,
            params.runtime,
            TransportIdx(0),
            params.channels,
            params.intern_events,
            params.new_data,
        ))]
    }
}

impl<A, B> AsyncServerTransports for (AsyncTransport<A>, AsyncTransport<B>)
where
    A: ServerTransport + Send + Sync + 'static,
    B: ServerTransport + Send + Sync + 'static,
{
    fn start(self: Box<Self>, params: ServerTransportsParams) -> Vec<Arc<RunnerHandle>> {
        let (a, b) = *self;
        vec![
            Arc::new(runner::start(
                a,
                Arc::clone(&params.runtime),
                TransportIdx(0),
                Arc::clone(&params.channels),
                params.intern_events.clone(),
                Arc::clone(&params.new_data),
            )),
            Arc::new(runner::start(
                b,
                params.runtime,
                TransportIdx(1),
                params.channels,
                params.intern_events,
                params.new_data,
            )),
        ]
    }
}

impl<A, B, C> AsyncServerTransports for (AsyncTransport<A>, AsyncTransport<B>, AsyncTransport<C>)
where
    A: ServerTransport + Send + Sync + 'static,
    B: ServerTransport + Send + Sync + 'static,
    C: ServerTransport + Send + Sync + 'static,
{
    fn start(self: Box<Self>, params: ServerTransportsParams) -> Vec<Arc<RunnerHandle>> {
        let (a, b, c) = *self;
        vec![
            Arc::new(runner::start(
                a,
                Arc::clone(&params.runtime),
                TransportIdx(0),
                Arc::clone(&params.channels),
                params.intern_events.clone(),
                Arc::clone(&params.new_data),
            )),
            Arc::new(runner::start(
                b,
                Arc::clone(&params.runtime),
                TransportIdx(1),
                Arc::clone(&params.channels),
                params.intern_events.clone(),
                Arc::clone(&params.new_data),
            )),
            Arc::new(runner::start(
                c,
                params.runtime,
                TransportIdx(2),
                params.channels,
                params.intern_events,
                params.new_data,
            )),
        ]
    }
}

// TODO: more tuples ...
