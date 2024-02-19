mod runner;

use self::runner::RunnerHandle;
use crate::{
    channel::Channels,
    connection::ConnectionState,
    new_data::NewDataAvailable,
    transport::{AsyncTransport, ClientTransport},
    NetworkDecode, NetworkEncode, NetworkError, NetworkMessage, Runtime,
};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

enum ClientState {
    Disconnected,
    Running {
        runner: RunnerHandle,
        intern_recv: UnboundedReceiver<InternEvent>,
        connection: Option<Arc<ConnectionState>>,
        new_data: Arc<NewDataAvailable>,
    },
}

#[cfg_attr(feature = "bevy", derive(bevy_ecs::system::Resource))]
pub struct Client {
    channels: Arc<Channels>,
    state: ClientState,
}

impl Client {
    pub fn new(channels: Arc<Channels>) -> Self {
        Self { channels, state: ClientState::Disconnected }
    }

    pub fn connect(&mut self, transport: impl AsyncClientTransport, runtime: Arc<dyn Runtime>) {
        self.disconnect();

        let (intern_send, intern_recv) = mpsc::unbounded_channel();
        let new_data = Arc::new(NewDataAvailable::new());

        let runner = Box::new(transport).start(ClientTransportsParams {
            channels: Arc::clone(&self.channels),
            intern_events: InternEventSender { intern_send, new_data: Arc::clone(&new_data) },
            new_data: Arc::clone(&new_data),
            runtime,
        });

        self.state = ClientState::Running { runner, intern_recv, connection: None, new_data };
    }

    pub fn disconnect(&mut self) {
        if let ClientState::Running { runner, .. } = &self.state {
            runner.disconnect(true);
        }

        self.state = ClientState::Disconnected;
    }

    pub fn process_events(&mut self) -> impl Iterator<Item = ClientEvent> + '_ {
        ProcessEvents(&mut self.state)
    }

    pub fn send<T: NetworkEncode + NetworkMessage + Send + 'static>(&self, message: T) {
        self.channels.assert_send_exists(T::CHANNEL_ID);
        if let ClientState::Running { runner, .. } = &self.state {
            runner.send(Box::new(message), T::CHANNEL_ID, T::CHANNEL_CONFIG);
        }
    }

    pub fn recv<T: NetworkDecode + NetworkMessage + 'static>(
        &self,
    ) -> impl Iterator<Item = T> + '_ {
        match &self.state {
            ClientState::Running { connection, .. } => match connection {
                Some(connection) => RecvIterator::Receiver(connection.get_receiver()),
                None => RecvIterator::Disconnected,
            },
            ClientState::Disconnected => RecvIterator::Disconnected,
        }
    }

    pub fn is_running(&self) -> bool {
        match self.state {
            ClientState::Disconnected => false,
            ClientState::Running { .. } => true,
        }
    }

    pub fn wait_timeout(&self, duration: Duration) -> bool {
        match &self.state {
            ClientState::Disconnected => false,
            ClientState::Running { new_data, .. } => new_data.wait_timeout(duration),
        }
    }
}

#[derive(Debug)]
#[cfg_attr(feature = "bevy", derive(bevy_ecs::event::Event))]
pub enum ClientEvent {
    Connected,
    Disconnected(Option<NetworkError>),
    Error(NetworkError),
}

enum InternEvent {
    Connected(Arc<ConnectionState>),
    Disconnected(Option<NetworkError>),
    Error(NetworkError),
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
    Disconnected,
    Receiver(&'a Mutex<UnboundedReceiver<T>>),
}

impl<T> Iterator for RecvIterator<'_, T> {
    type Item = T;

    fn next(&mut self) -> Option<Self::Item> {
        match &*self {
            Self::Disconnected => None,
            Self::Receiver(recv) => recv.lock().unwrap().try_recv().ok(),
        }
    }
}

struct ProcessEvents<'a>(&'a mut ClientState);

impl Iterator for ProcessEvents<'_> {
    type Item = ClientEvent;

    fn next(&mut self) -> Option<Self::Item> {
        match self.0 {
            ClientState::Disconnected => None,
            ClientState::Running { intern_recv, connection, .. } => {
                let event = intern_recv.try_recv().ok()?;

                Some(match event {
                    InternEvent::Connected(conn) => {
                        *connection = Some(conn);
                        ClientEvent::Connected
                    }
                    InternEvent::Disconnected(err) => {
                        *self.0 = ClientState::Disconnected;
                        ClientEvent::Disconnected(err)
                    }
                    InternEvent::Error(error) => ClientEvent::Error(error),
                })
            }
        }
    }
}

pub struct ClientTransportsParams {
    channels: Arc<Channels>,
    intern_events: InternEventSender,
    new_data: Arc<NewDataAvailable>,
    runtime: Arc<dyn Runtime>,
}

pub trait AsyncClientTransport {
    fn start(self: Box<Self>, params: ClientTransportsParams) -> RunnerHandle;
}

impl AsyncClientTransport for Box<dyn AsyncClientTransport + Send + Sync + 'static> {
    fn start(self: Box<Self>, params: ClientTransportsParams) -> RunnerHandle {
        (*self).start(params)
    }
}

impl<T> AsyncClientTransport for AsyncTransport<T>
where
    T: ClientTransport + Send + Sync + 'static,
{
    fn start(self: Box<Self>, params: ClientTransportsParams) -> RunnerHandle {
        let transport = *self;
        runner::start(
            transport,
            params.runtime,
            params.channels,
            params.intern_events,
            params.new_data,
        )
    }
}
