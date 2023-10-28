use crate::{handle::ConnectionIdx, transport::Transport};
use tinyvec::ArrayVec;
use uuid::Uuid;

pub const MAX_CHANNEL_ID: u8 = 127;

pub fn max_payload_size<T: Transport>() -> usize {
    *T::MAX_PACKET_SIZE - 1
}

pub enum InternalS2C {
    Connect(ConnectionIdx, Uuid),
    Disconnect,
    RequestId,
}

impl InternalS2C {
    pub fn decode(buf: &[u8]) -> Option<Self> {
        let kind = *buf.first()?;
        match (kind, buf.len() - 1) {
            (128, 20) => {
                let connection_idx =
                    ConnectionIdx(u32::from_le_bytes(buf[0..4].try_into().unwrap()));
                let uuid = Uuid::from_bytes(buf[4..20].try_into().unwrap());
                Some(Self::Connect(connection_idx, uuid))
            }
            (129, 0) => Some(Self::Disconnect),
            (130, 0) => Some(Self::RequestId),
            (_, _) => None,
        }
    }

    pub fn encode(&self) -> ArrayVec<[u8; 21]> {
        match self {
            Self::Connect(connection_idx, uuid) => {
                let mut buf = ArrayVec::new();
                buf.push(128);
                buf.extend_from_slice(&u32::to_le_bytes(connection_idx.0));
                buf.extend_from_slice(uuid.as_bytes());
                buf
            }
            Self::Disconnect => ArrayVec::from_iter([129]),
            Self::RequestId => ArrayVec::from_iter([130]),
        }
    }
}

pub enum InternalC2S {
    Connect,
    Disconnect,
    ProvideId(ConnectionIdx, Uuid),
}

impl InternalC2S {
    pub fn decode(buf: &[u8]) -> Option<Self> {
        let kind = *buf.first()?;
        match (kind, buf.len() - 1) {
            (128, 0) => Some(Self::Connect),
            (129, 0) => Some(Self::Disconnect),
            (130, 20) => {
                let connection_idx =
                    ConnectionIdx(u32::from_le_bytes(buf[0..4].try_into().unwrap()));
                let uuid = Uuid::from_bytes(buf[4..20].try_into().unwrap());
                Some(Self::ProvideId(connection_idx, uuid))
            }
            (_, _) => None,
        }
    }

    pub fn encode(&self) -> ArrayVec<[u8; 21]> {
        match self {
            Self::Connect => ArrayVec::from_iter([128]),
            Self::Disconnect => ArrayVec::from_iter([129]),
            Self::ProvideId(connection_idx, uuid) => {
                let mut buf = ArrayVec::new();
                buf.push(130);
                buf.extend_from_slice(&u32::to_le_bytes(connection_idx.0));
                buf.extend_from_slice(uuid.as_bytes());
                buf
            }
        }
    }
}
