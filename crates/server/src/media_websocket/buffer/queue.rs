use super::{BufferItem, BufferNotice};
use rusterisk_media::{Frame, MediaError, PcmFrame};
use std::collections::VecDeque;

pub(super) enum Slot<const N: usize> {
    Audio(PcmFrame<N>),
    Notice(BufferNotice),
}

impl<const N: usize> Slot<N> {
    fn output(self) -> BufferItem {
        match self {
            Self::Audio(frame) => BufferItem::Frame(frame.expanded()),
            Self::Notice(notice) => BufferItem::Notice(notice),
        }
    }
}

macro_rules! queues {
    ($( $kind:ident($rate:literal, $samples:literal) ),+ $(,)?) => {
        pub(super) enum PlayoutQueue {
            $( $kind(VecDeque<Slot<$samples>>), )+
        }

        impl PlayoutQueue {
            pub(super) fn new(rate: u32) -> Result<Self, MediaError> {
                match rate {
                    $( $rate => Ok(Self::$kind(VecDeque::with_capacity(16))), )+
                    _ => Err(MediaError::InvalidFrame),
                }
            }

            pub(super) fn audio(&mut self, frame: &Frame) -> Result<(), MediaError> {
                match self { $( Self::$kind(slots) => slots.push_back(Slot::Audio(frame.fit()?)), )+ }
                Ok(())
            }

            pub(super) fn notice(&mut self, notice: BufferNotice) {
                match self { $( Self::$kind(slots) => slots.push_back(Slot::Notice(notice)), )+ }
            }

            pub(super) fn pop(&mut self) -> Option<BufferItem> {
                match self { $( Self::$kind(slots) => slots.pop_front().map(Slot::output), )+ }
            }

            pub(super) fn clear(&mut self) {
                match self { $( Self::$kind(slots) => slots.clear(), )+ }
            }
        }
    };
}

crate::media_link::pcm_rates!(queues);
