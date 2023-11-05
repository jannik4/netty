use bevy_app::{App, First, Last};
use bevy_ecs::{
    event::{Event, EventWriter, Events},
    schedule::IntoSystemConfigs,
    system::{Res, ResMut, Resource},
};
use netty::{
    Channels, ConnectionHandle, NetworkDecode, NetworkEncode, NetworkMessage, Server, ServerEvent,
};
use std::sync::Arc;

#[derive(Debug, Event)]
pub struct ToClient<T>(pub T, pub ConnectionHandle);

#[derive(Debug, Event)]
pub struct Broadcast<T>(pub T);

#[derive(Debug, Event)]
pub struct FromClient<T>(pub T, pub ConnectionHandle);

#[derive(Resource, Clone)]
pub struct ServerChannels(pub Arc<Channels>);

pub struct ServerChannelsBuilder<'a>(&'a mut App, Channels);

impl ServerChannelsBuilder<'_> {
    pub fn add_recv<T>(&mut self) -> &mut Self
    where
        T: NetworkDecode + NetworkMessage + Send + Sync + 'static,
    {
        self.0.add_event::<FromClient<T>>();
        self.0.add_systems(
            First,
            handle_recv::<T>
                .after(<Events<FromClient<T>>>::update_system)
                .after(process_server_events),
        );

        self.1.add_recv::<T>();

        self
    }

    pub fn add_send<T>(&mut self) -> &mut Self
    where
        T: NetworkEncode + NetworkMessage + Send + Sync + 'static,
    {
        self.0.add_event::<ToClient<T>>();
        self.0.add_event::<Broadcast<T>>();
        self.0.add_systems(Last, handle_send::<T>);

        self.1.add_send::<T>();

        self
    }
}

pub trait ServerAppExt {
    fn register_server_channels(
        &mut self,
        f: impl FnOnce(&mut ServerChannelsBuilder),
    ) -> ServerChannels;
}

impl ServerAppExt for App {
    fn register_server_channels(
        &mut self,
        f: impl FnOnce(&mut ServerChannelsBuilder),
    ) -> ServerChannels {
        let mut builder = ServerChannelsBuilder(self, Channels::new());
        f(&mut builder);
        let channels = ServerChannels(Arc::new(builder.1));
        self.insert_resource(channels.clone());
        channels
    }
}

pub(super) fn process_server_events(
    mut server: Option<ResMut<Server>>,
    mut events: EventWriter<ServerEvent>,
) {
    let Some(server) = server.as_mut() else {
        return;
    };
    for event in server.process_events() {
        events.send(event);
    }
}

fn handle_recv<T>(server: Option<Res<Server>>, mut events: EventWriter<FromClient<T>>)
where
    T: NetworkDecode + NetworkMessage + Send + Sync + 'static,
{
    let Some(server) = server.as_ref() else {
        return;
    };
    for (message, handle) in server.recv::<T>() {
        events.send(FromClient(message, handle));
    }
}

fn handle_send<T>(
    server: Option<Res<Server>>,
    mut events_to_client: ResMut<Events<ToClient<T>>>,
    mut events_broadcast: ResMut<Events<Broadcast<T>>>,
) where
    T: NetworkEncode + NetworkMessage + Send + Sync + 'static,
{
    let Some(server) = server.as_ref() else {
        return;
    };
    for event in events_to_client.drain() {
        server.send_to(event.0, event.1);
    }
    for event in events_broadcast.drain() {
        server.broadcast(event.0);
    }
}
