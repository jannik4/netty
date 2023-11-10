use bevy_app::{App, First, Last};
use bevy_ecs::{
    event::{event_update_system, Event, EventWriter, Events},
    schedule::IntoSystemConfigs,
    system::{Res, ResMut, Resource},
};
use netty::{Channels, Client, ClientEvent, NetworkDecode, NetworkEncode, NetworkMessage};
use std::sync::Arc;

#[derive(Debug, Event)]
pub struct ToServer<T>(pub T);

#[derive(Debug, Event)]
pub struct FromServer<T>(pub T);

#[derive(Resource, Clone)]
pub struct ClientChannels(pub Arc<Channels>);

pub struct ClientChannelsBuilder<'a>(&'a mut App, Channels);

impl ClientChannelsBuilder<'_> {
    pub fn add_recv<T>(&mut self) -> &mut Self
    where
        T: NetworkDecode + NetworkMessage + Send + Sync + 'static,
    {
        self.0.add_event::<FromServer<T>>();
        self.0.add_systems(
            First,
            handle_recv::<T>
                .after(event_update_system::<FromServer<T>>)
                .after(process_client_events),
        );

        self.1.add_recv::<T>();

        self
    }

    pub fn add_send<T>(&mut self) -> &mut Self
    where
        T: NetworkEncode + NetworkMessage + Send + Sync + 'static,
    {
        self.0.add_event::<ToServer<T>>();
        self.0.add_systems(Last, handle_send::<T>);

        self.1.add_send::<T>();

        self
    }
}

pub trait ClientAppExt {
    fn register_client_channels(
        &mut self,
        f: impl FnOnce(&mut ClientChannelsBuilder),
    ) -> ClientChannels;
}

impl ClientAppExt for App {
    fn register_client_channels(
        &mut self,
        f: impl FnOnce(&mut ClientChannelsBuilder),
    ) -> ClientChannels {
        let mut builder = ClientChannelsBuilder(self, Channels::new());
        f(&mut builder);
        let channels = ClientChannels(Arc::new(builder.1));
        self.insert_resource(channels.clone());
        channels
    }
}

pub(super) fn process_client_events(
    mut client: Option<ResMut<Client>>,
    mut events: EventWriter<ClientEvent>,
) {
    let Some(client) = client.as_mut() else {
        return;
    };
    for event in client.process_events() {
        events.send(event);
    }
}

fn handle_recv<T>(client: Option<Res<Client>>, mut events: EventWriter<FromServer<T>>)
where
    T: NetworkDecode + NetworkMessage + Send + Sync + 'static,
{
    let Some(client) = client.as_ref() else {
        return;
    };
    for event in client.recv::<T>() {
        events.send(FromServer(event));
    }
}

fn handle_send<T>(client: Option<Res<Client>>, mut events: ResMut<Events<ToServer<T>>>)
where
    T: NetworkEncode + NetworkMessage + Send + Sync + 'static,
{
    let Some(client) = client.as_ref() else {
        return;
    };
    for event in events.drain() {
        client.send(event.0);
    }
}
