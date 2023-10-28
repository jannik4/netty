use crate::{
    transport::TransportProperties, Channel, ChannelId, Channels, DecodeError, NetworkDecode,
    NetworkMessage,
};
use crossbeam_channel::Receiver;
use std::{any::Any, collections::HashMap};

pub struct ConnectionState {
    transport_properties: TransportProperties,

    send: HashMap<ChannelId, ChannelSend>,
    recv: HashMap<ChannelId, ChannelRecv>,
}

impl ConnectionState {
    pub(crate) fn new(transport_properties: TransportProperties, channels: &Channels) -> Self {
        Self {
            transport_properties,
            send: channels
                .send
                .iter()
                .map(|(id, channel)| (*id, ChannelSend { channel: channel.clone() }))
                .collect(),
            recv: channels
                .recv
                .iter()
                .map(|(id, (channel, factory))| {
                    let (decode, recv) = factory.0();
                    (*id, ChannelRecv { channel: channel.clone(), decode, recv })
                })
                .collect(),
        }
    }

    pub(crate) fn get_receiver<T>(&self) -> &Receiver<T>
    where
        T: NetworkDecode + NetworkMessage + 'static,
    {
        let channel = self
            .recv
            .get(&T::CHANNEL_ID)
            .unwrap_or_else(|| panic!("no channel registered for message with {}", T::CHANNEL_ID));

        channel.recv.downcast_ref().unwrap_or_else(|| {
            panic!(
                "the type used to receive messages from {}\
                does not match the registered type: (expected: {}, actual: {})",
                T::CHANNEL_ID,
                channel.channel.ty_name,
                std::any::type_name::<T>(),
            )
        })
    }

    pub(crate) fn decode_recv(
        &self,
        channel_id: ChannelId,
        message: &[u8],
    ) -> Option<Result<(), DecodeError>> {
        let channel = self.recv.get(&channel_id)?;
        Some((channel.decode)(message))
    }

    pub fn transport_properties(&self) -> &TransportProperties {
        &self.transport_properties
    }
}

struct ChannelSend {
    channel: Channel,
}

struct ChannelRecv {
    channel: Channel,

    decode: Box<dyn Fn(&[u8]) -> Result<(), DecodeError> + Send + Sync>,
    recv: Box<dyn Any + Send + Sync>,
}

pub(crate) struct DecodeRecvFactory(
    Box<
        dyn Fn() -> (
                Box<dyn Fn(&[u8]) -> Result<(), DecodeError> + Send + Sync>,
                Box<dyn Any + Send + Sync>,
            ) + Send
            + Sync,
    >,
);

impl DecodeRecvFactory {
    pub(crate) fn new<T>() -> Self
    where
        T: NetworkDecode + NetworkMessage + Send + 'static,
    {
        Self(Box::new(|| {
            let (sender, receiver) = crossbeam_channel::unbounded();
            let decode = Box::new(move |buf: &[u8]| {
                let message = T::decode(buf)?;
                sender.send(message).ok();
                Ok(())
            });
            let recv = Box::new(receiver);
            (decode, recv)
        }))
    }
}
