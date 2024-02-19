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

pub type BevyNettyRuntime = runtime::BevyNettyRuntime;

pub fn runtime() -> BevyNettyRuntime {
    runtime::get()
}

#[cfg(target_arch = "wasm32")]
mod runtime {
    use netty::{NettyFuture, Runtime};
    use std::time::Duration;

    pub type BevyNettyRuntime = &'static IoTaskPoolWrapper;

    pub fn get() -> BevyNettyRuntime {
        unsafe {
            std::mem::transmute::<&'static bevy_tasks::IoTaskPool, &'static IoTaskPoolWrapper>(
                bevy_tasks::IoTaskPool::get(),
            )
        }
    }

    #[derive(Debug)]
    #[repr(transparent)]
    pub struct IoTaskPoolWrapper(&'static bevy_tasks::IoTaskPool);

    impl Runtime for &'static IoTaskPoolWrapper {
        fn spawn_boxed(&self, f: NettyFuture<()>) {
            self.0.spawn(f).detach();
        }
        fn sleep(&self, duration: Duration) -> NettyFuture<()> {
            Box::pin(gloo_timers::future::TimeoutFuture::new(duration.as_millis() as u32))
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
mod runtime {
    use std::sync::OnceLock;

    pub type BevyNettyRuntime = &'static tokio::runtime::Runtime;

    // TODO: Make configurable
    pub fn get() -> &'static tokio::runtime::Runtime {
        static INSTANCE: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
        INSTANCE.get_or_init(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .enable_all()
                .build()
                .unwrap()
        })
    }
}
