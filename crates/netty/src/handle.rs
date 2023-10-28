#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ConnectionHandle {
    pub(crate) transport_idx: TransportIdx,
    pub(crate) connection_idx: ConnectionIdx,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TransportIdx(pub(crate) u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ConnectionIdx(pub(crate) u32);
