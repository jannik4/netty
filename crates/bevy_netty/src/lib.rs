pub mod client;
pub mod server;

use bevy_app::{App, Plugin, PreUpdate};
use netty::{ClientEvent, ServerEvent};

pub use self::client::{FromServer, ToServer};
pub use netty;

pub struct NettyPlugin;

impl Plugin for NettyPlugin {
    fn build(&self, app: &mut App) {
        app.add_event::<ServerEvent>().add_systems(PreUpdate, server::process_server_events);
        app.add_event::<ClientEvent>().add_systems(PreUpdate, client::process_client_events);
    }
}
