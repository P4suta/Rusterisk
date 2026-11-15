use crate::AudioGate;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PeerMediaDirection {
    SendReceive,
    SendOnly,
    ReceiveOnly,
    Inactive,
}

impl PeerMediaDirection {
    pub const fn can_send(self) -> bool {
        match self {
            Self::SendReceive | Self::SendOnly => true,
            Self::ReceiveOnly | Self::Inactive => false,
        }
    }

    pub const fn can_receive(self) -> bool {
        match self {
            Self::SendReceive | Self::ReceiveOnly => true,
            Self::SendOnly | Self::Inactive => false,
        }
    }

    pub(crate) const fn restrict(self, local: AudioGate) -> AudioGate {
        match self {
            Self::SendReceive => local,
            Self::Inactive => AudioGate::Closed,
            Self::SendOnly => match local {
                AudioGate::Open | AudioGate::OutgoingMuted => AudioGate::OutgoingMuted,
                AudioGate::IncomingMuted | AudioGate::Closed => AudioGate::Closed,
            },
            Self::ReceiveOnly => match local {
                AudioGate::Open | AudioGate::IncomingMuted => AudioGate::IncomingMuted,
                AudioGate::OutgoingMuted | AudioGate::Closed => AudioGate::Closed,
            },
        }
    }
}
