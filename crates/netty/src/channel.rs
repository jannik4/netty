use crate::{
    connection::DecodeRecvFactory, Channel, ChannelId, NetworkDecode, NetworkEncode, NetworkMessage,
};
use std::{
    any::{self, TypeId},
    collections::HashMap,
};

pub struct Channels {
    pub(crate) send: HashMap<ChannelId, Channel>,
    pub(crate) recv: HashMap<ChannelId, (Channel, DecodeRecvFactory)>,
}

impl Channels {
    pub fn new() -> Self {
        Self { send: HashMap::new(), recv: HashMap::new() }
    }

    pub fn add_send<T>(&mut self) -> &mut Self
    where
        T: NetworkEncode + NetworkMessage + 'static,
    {
        let old = self.send.insert(
            T::CHANNEL_ID,
            Channel {
                id: T::CHANNEL_ID,
                config: T::CHANNEL_CONFIG,
                ty: TypeId::of::<T>(),
                ty_name: any::type_name::<T>(),
            },
        );
        if let Some(old) = old {
            panic!("{} already exists: {}", T::CHANNEL_ID, old.ty_name);
        }

        self
    }

    pub fn add_recv<T>(&mut self) -> &mut Self
    where
        T: NetworkDecode + NetworkMessage + Send + 'static,
    {
        let old = self.recv.insert(
            T::CHANNEL_ID,
            (
                Channel {
                    id: T::CHANNEL_ID,
                    config: T::CHANNEL_CONFIG,
                    ty: TypeId::of::<T>(),
                    ty_name: any::type_name::<T>(),
                },
                DecodeRecvFactory::new::<T>(),
            ),
        );
        if let Some((old, _)) = old {
            panic!("{} already exists: {}", T::CHANNEL_ID, old.ty_name);
        }

        self
    }

    // TODO: ???
    pub(crate) fn assert_send_exists(&self, id: ChannelId) {
        if !self.send.contains_key(&id) {
            panic!("{} does not exist", id);
        }
    }
}

impl Default for Channels {
    fn default() -> Self {
        Self::new()
    }
}
