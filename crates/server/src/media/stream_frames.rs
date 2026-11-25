use rusterisk_media::{Frame, MediaError, PcmFrame};
use tokio::sync::mpsc;

pub(super) struct FramePair<const N: usize> {
    pub(super) incoming: mpsc::Receiver<PcmFrame<N>>,
    pub(super) outgoing: mpsc::Sender<PcmFrame<N>>,
}

impl<const N: usize> FramePair<N> {
    fn receive(&mut self) -> Result<Frame, mpsc::error::TryRecvError> {
        self.incoming.try_recv().map(|frame| frame.expanded())
    }

    fn send(&self, frame: &Frame) -> Result<(), MediaError> {
        let slot = self.outgoing.try_reserve().map_err(|error| match error {
            mpsc::error::TrySendError::Full(()) => MediaError::Capacity,
            mpsc::error::TrySendError::Closed(()) => MediaError::UnknownStream,
        })?;
        let rate = N as u32 * 50;
        let output = if frame.rate() == rate {
            frame.fit()?
        } else {
            frame.resample(rate)?.fit()?
        };
        slot.send(output);
        Ok(())
    }
}

macro_rules! stream_rates {
    ($( $variant:ident($rate:literal, $samples:literal) ),+ $(,)?) => {
        pub(super) enum TaskFrames {
            $( $variant(FramePair<$samples>), )+
        }

        impl TaskFrames {
            pub(super) fn receive(&mut self) -> Result<Frame, mpsc::error::TryRecvError> {
                match self { $(Self::$variant(pair) => pair.receive(),)+ }
            }

            pub(super) fn send(&self, frame: &Frame) -> Result<(), MediaError> {
                match self { $(Self::$variant(pair) => pair.send(frame),)+ }
            }

            #[cfg(test)]
            pub(super) fn frame_bytes(&self) -> usize {
                match self { $(Self::$variant(_) => size_of::<PcmFrame<$samples>>(),)+ }
            }
        }

        macro_rules! with_stream_port {
            ($selected:expr, |$port:ident, $channels:ident| $body:block) => {
                match $selected {
                    $( $rate => {
                        let ($port, $channels) = Port::with_frames(
                            PeerMediaDirection::SendReceive, DigitSupport::Stream, TaskFrames::$variant
                        );
                        $body
                    }, )+
                    _ => Err(MediaError::InvalidFrame),
                }
            };
        }
    };
}

stream_rates!(
    Hz8000(8000, 160),
    Hz12000(12000, 240),
    Hz16000(16000, 320),
    Hz24000(24000, 480),
    Hz32000(32000, 640),
    Hz44100(44100, 882),
    Hz48000(48000, 960),
);
pub(super) use with_stream_port;
