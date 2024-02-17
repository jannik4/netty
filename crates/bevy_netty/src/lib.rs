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

pub fn runtime() -> impl netty::Runtime {
    runtime::get()
}

#[cfg(target_arch = "wasm32")]
mod runtime {
    use netty::{Runtime, WasmNotSend};
    use std::{future::Future, time::Duration};

    pub fn get() -> impl netty::Runtime {
        unsafe {
            std::mem::transmute::<&'static bevy_tasks::IoTaskPool, &'static BevyNettyRuntime>(
                bevy_tasks::IoTaskPool::get(),
            )
        }
    }

    #[derive(Debug)]
    #[repr(transparent)]
    struct BevyNettyRuntime(&'static bevy_tasks::IoTaskPool);

    impl Runtime for &'static BevyNettyRuntime {
        fn spawn<F>(&self, f: F)
        where
            F: Future<Output = ()> + WasmNotSend + 'static,
        {
            self.0.spawn(f).detach();
        }

        async fn sleep(&self, duration: Duration) {
            gloo_timers::future::TimeoutFuture::new(duration.as_millis() as u32).await;
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
mod runtime {
    use netty::NativeRuntime;
    use std::sync::OnceLock;

    // TODO: Make configurable
    pub fn get() -> impl netty::Runtime {
        static INSTANCE: OnceLock<NativeRuntime> = OnceLock::new();
        INSTANCE.get_or_init(|| {
            NativeRuntime(
                tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(1)
                    .enable_all()
                    .build()
                    .unwrap(),
            )
        })
    }
}
