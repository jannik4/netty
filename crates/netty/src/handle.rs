#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ConnectionHandle {
    pub(crate) transport_idx: TransportIdx,
    pub(crate) connection_idx: ConnectionIdx,
}

impl ConnectionHandle {
    pub fn as_u64(self) -> u64 {
        (self.transport_idx.0 as u64) << 32 | self.connection_idx.0 as u64
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TransportIdx(pub(crate) u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ConnectionIdx(pub(crate) u32);
