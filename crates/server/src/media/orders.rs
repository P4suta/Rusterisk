use super::{Command, Completion, MediaPort, RecordingEnd};
use crate::CallKey;
use rusterisk_core::{MediaTaskToken, PeerMediaDirection};
use rusterisk_media::{Gate, MediaError};
use tokio::sync::mpsc;

// A media command that needs no answer.
// Signaling keeps these in their order while the media queue is full.
pub(crate) enum Order {
    Release(CallKey),
    Bridge {
        id: u64,
        members: Vec<CallKey>,
    },
    ReleaseBridge(u64),
    Gate {
        key: CallKey,
        gate: Gate,
        generated_audio_gate: Gate,
    },
    EndRecording {
        token: MediaTaskToken,
        mode: RecordingEnd,
    },
    // The media task reports the result as a notice with the same number.
    PeerDirection {
        key: CallKey,
        direction: PeerMediaDirection,
        gate: Gate,
        generated_audio_gate: Gate,
        reply: Completion<bool>,
    },
}

impl Order {
    // Checks the membership before the order waits, so that its later send can't fail.
    pub(crate) fn bridge(id: u64, members: Vec<CallKey>) -> Result<Self, MediaError> {
        if members.len() > 64 {
            return Err(MediaError::Capacity);
        }
        Ok(Self::Bridge { id, members })
    }

    pub(super) fn command(self) -> Command {
        match self {
            Self::Release(key) => Command::Release { key },
            Self::Bridge { id, members } => Command::Bridge { id, members },
            Self::ReleaseBridge(id) => Command::ReleaseBridge { id },
            Self::Gate {
                key,
                gate,
                generated_audio_gate,
            } => Command::Gate {
                key,
                gate,
                generated_audio_gate,
            },
            Self::EndRecording { token, mode } => Command::EndRecording { token, mode },
            Self::PeerDirection {
                key,
                direction,
                gate,
                generated_audio_gate,
                reply,
            } => Command::PeerDirection {
                key,
                direction,
                gate,
                generated_audio_gate,
                reply,
            },
        }
    }
}

impl MediaPort {
    pub(crate) fn direction_order(
        &self,
        key: CallKey,
        direction: PeerMediaDirection,
        gate: Gate,
        generated_audio_gate: Gate,
        serial: u64,
    ) -> Result<Order, MediaError> {
        Ok(Order::PeerDirection {
            key,
            direction,
            gate,
            generated_audio_gate,
            reply: self.completion(serial)?,
        })
    }

    // Sends an order now, or returns it while the media queue is full.
    pub(crate) fn offer(&self, order: Order) -> Result<Option<Order>, MediaError> {
        match self.commands.try_reserve() {
            Ok(permit) => {
                permit.send(order.command());
                Ok(None)
            }
            Err(mpsc::error::TrySendError::Full(())) => Ok(Some(order)),
            Err(mpsc::error::TrySendError::Closed(())) => Err(MediaError::ServiceClosed),
        }
    }

    // Sends an order now, and fails while the media queue is full.
    pub(crate) fn send(&self, order: Order) -> Result<(), MediaError> {
        match self.offer(order)? {
            None => Ok(()),
            Some(_) => Err(MediaError::Capacity),
        }
    }

    // Waits until the media queue has room for an order, and leaves the room to the next offer.
    pub(crate) async fn room(&self) -> Result<(), MediaError> {
        self.commands
            .reserve()
            .await
            .map(drop)
            .map_err(|_| MediaError::ServiceClosed)
    }
}
