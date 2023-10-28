mod runner;

use self::runner::RunnerHandle;
use crate::{
    channel::Channels, connection::ConnectionState, transport::ClientTransport, NetworkDecode,
    NetworkEncode, NetworkError, NetworkMessage,
};
use crossbeam_channel::Receiver;
use std::sync::Arc;

enum ClientState {
    Disconnected,
    Running {
        runner: RunnerHandle,
        intern_recv: Receiver<InternEvent>,
        connection: Arc<ConnectionState>,
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

    pub fn connect<T: ClientTransport + Send + Sync + 'static>(&mut self, transport: T) {
        self.disconnect();

        let connection = Arc::new(ConnectionState::new(T::TRANSPORT_PROPERTIES, &self.channels));
        let (intern_send, intern_recv) = crossbeam_channel::unbounded();

        let runner = runner::start(transport, Arc::clone(&connection), intern_send);

        self.state = ClientState::Running { runner, intern_recv, connection };
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
            ClientState::Running { connection, .. } => {
                RecvIterator::Receiver(connection.get_receiver())
            }
            ClientState::Disconnected => RecvIterator::Disconnected,
        }
    }

    pub fn is_running(&self) -> bool {
        match self.state {
            ClientState::Disconnected => false,
            ClientState::Running { .. } => true,
        }
    }
}

#[derive(Debug)]
#[cfg_attr(feature = "bevy", derive(bevy_ecs::event::Event))]
pub enum ClientEvent {
    Connected,
    Disconnected,
    Error(NetworkError),
}

enum InternEvent {
    Connected,
    Disconnected,
    Error(NetworkError),
}

enum RecvIterator<'a, T> {
    Disconnected,
    Receiver(&'a Receiver<T>),
}

impl<T> Iterator for RecvIterator<'_, T> {
    type Item = T;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Disconnected => None,
            Self::Receiver(recv) => recv.try_recv().ok(),
        }
    }
}

struct ProcessEvents<'a>(&'a mut ClientState);

impl Iterator for ProcessEvents<'_> {
    type Item = ClientEvent;

    fn next(&mut self) -> Option<Self::Item> {
        match self.0 {
            ClientState::Disconnected => None,
            ClientState::Running { intern_recv, .. } => {
                let event = intern_recv.try_recv().ok()?;

                Some(match event {
                    InternEvent::Connected => ClientEvent::Connected,
                    InternEvent::Disconnected => {
                        *self.0 = ClientState::Disconnected;
                        ClientEvent::Disconnected
                    }
                    InternEvent::Error(error) => ClientEvent::Error(error),
                })
            }
        }
    }
}
