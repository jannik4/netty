use crate::protocol;
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ChannelId(u8);

impl ChannelId {
    pub const fn new(id: u8) -> Self {
        match Self::try_new(id) {
            Some(channel_id) => channel_id,
            None => panic!("invalid channel id"),
        }
    }

    pub(crate) const fn try_new(id: u8) -> Option<Self> {
        if id <= protocol::MAX_CHANNEL_ID {
            Some(Self(id))
        } else {
            None
        }
    }

    pub(crate) const fn value(self) -> u8 {
        self.0
    }
}

impl fmt::Display for ChannelId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Channel({})", self.0)
    }
}
