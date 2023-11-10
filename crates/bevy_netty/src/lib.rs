pub mod client;
pub mod server;

use bevy_app::{App, First, Plugin};
use bevy_ecs::{event::event_update_system, schedule::IntoSystemConfigs};
use netty::{ClientEvent, ServerEvent};

pub use self::{
    client::{FromServer, ToServer},
    server::{FromClient, ToClient},
};
pub use netty;

pub struct NettyPlugin;

impl Plugin for NettyPlugin {
    fn build(&self, app: &mut App) {
        app.add_event::<ServerEvent>().add_systems(
            First,
            server::process_server_events.after(event_update_system::<ServerEvent>),
        );
        app.add_event::<ClientEvent>().add_systems(
            First,
            client::process_client_events.after(event_update_system::<ClientEvent>),
        );
    }
}
