use rusterisk_api::MediaFormat;
use rusterisk_media::{alaw_decode, mulaw_decode, Frame, MediaError, MAX_SAMPLES};
use std::collections::VecDeque;

#[derive(Debug, PartialEq, Eq)]
pub enum BufferNotice {
    Mark(String),
    BufferingCompleted(String),
}

#[derive(Debug, PartialEq, Eq)]
#[expect(
    clippy::large_enum_variant,
    reason = "docs/adr/bound-websocket-media-queues-by-message-admission.md: Keep bounded frames inline with ordered markers."
)]
pub enum BufferItem {
    Frame(Frame),
    Notice(BufferNotice),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlowNotice {
    Xoff,
    Xon,
}

#[derive(Debug, PartialEq, Eq)]
pub struct BufferOutput {
    pub item: Option<BufferItem>,
    pub flow: Option<FlowNotice>,
    pub drained: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum BufferMode {
    Discrete,
    Continuous,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Playout {
    Running,
    Paused,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Flow {
    Open,
    Backpressured,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Drain {
    Idle,
    Requested,
}

pub struct MediaBuffer {
    format: MediaFormat,
    queue: VecDeque<BufferItem>,
    frames: usize,
    markers: usize,
    partial: [u8; 1920],
    used: usize,
    mode: BufferMode,
    playout: Playout,
    flow: Flow,
    drain: Drain,
}

impl MediaBuffer {
    pub fn new(format: MediaFormat) -> Result<Self, MediaError> {
        if format.optimal_frame_size() == 0 {
            return Err(MediaError::Unsupported);
        }
        Ok(Self {
            format,
            queue: VecDeque::with_capacity(16),
            frames: 0,
            markers: 0,
            partial: [0; 1920],
            used: 0,
            mode: BufferMode::Discrete,
            playout: Playout::Running,
            flow: Flow::Open,
            drain: Drain::Idle,
        })
    }

    fn decoded(&self) -> Result<Frame, MediaError> {
        let mut samples = [0; MAX_SAMPLES];
        let size = usize::from(self.format.optimal_frame_size());
        match self.format {
            MediaFormat::Pcmu => {
                for (sample, byte) in samples.iter_mut().zip(&self.partial[..size]) {
                    *sample = mulaw_decode(*byte);
                }
            }
            MediaFormat::Pcma => {
                for (sample, byte) in samples.iter_mut().zip(&self.partial[..size]) {
                    *sample = alaw_decode(*byte);
                }
            }
            MediaFormat::Pcm8
            | MediaFormat::Pcm12
            | MediaFormat::Pcm16
            | MediaFormat::Pcm24
            | MediaFormat::Pcm32
            | MediaFormat::Pcm44
            | MediaFormat::Pcm48 => {
                for (sample, bytes) in samples
                    .iter_mut()
                    .zip(self.partial[..size].as_chunks::<2>().0)
                {
                    *sample = i16::from_le_bytes([bytes[0], bytes[1]]);
                }
            }
            MediaFormat::Opus => return Err(MediaError::Unsupported),
        }
        Frame::new(
            &samples[..self.format.rate() as usize / 50],
            self.format.rate(),
        )
    }

    fn append_frame(&mut self, frame: Frame) -> Option<FlowNotice> {
        self.queue.push_back(BufferItem::Frame(frame));
        self.frames += 1;
        if self.flow == Flow::Open && self.frames >= 900 {
            self.flow = Flow::Backpressured;
            Some(FlowNotice::Xoff)
        } else {
            None
        }
    }

    pub fn push(&mut self, bytes: &[u8]) -> Result<Option<FlowNotice>, MediaError> {
        if bytes.len() > 65500 || self.flow == Flow::Backpressured {
            return Err(MediaError::Capacity);
        }
        let size = usize::from(self.format.optimal_frame_size());
        let count = (self.used + bytes.len()) / size;
        if self.frames + count > 1000 {
            return Err(MediaError::Capacity);
        }
        let mut offset = 0;
        let mut flow = None;
        while offset < bytes.len() {
            let count = (size - self.used).min(bytes.len() - offset);
            self.partial[self.used..self.used + count]
                .copy_from_slice(&bytes[offset..offset + count]);
            self.used += count;
            offset += count;
            if self.used == size {
                let frame = self.decoded()?;
                flow = self.append_frame(frame).or(flow);
                self.used = 0;
            }
        }
        if self.mode == BufferMode::Discrete {
            self.used = 0;
        }
        Ok(flow)
    }

    fn marker_capacity(&self, id: &str) -> Result<(), MediaError> {
        if self.markers >= 128 || id.len() > 128 {
            return Err(MediaError::Capacity);
        }
        if id.chars().any(char::is_control) {
            return Err(MediaError::InvalidPacket);
        }
        Ok(())
    }

    pub fn mark(&mut self, id: String) -> Result<(), MediaError> {
        self.marker_capacity(&id)?;
        self.queue
            .push_back(BufferItem::Notice(BufferNotice::Mark(id)));
        self.markers += 1;
        Ok(())
    }

    pub fn start_buffering(&mut self) {
        self.mode = BufferMode::Continuous;
    }

    pub fn finish_buffering(&mut self, id: String) -> Result<Option<FlowNotice>, MediaError> {
        self.marker_capacity(&id)?;
        if self.used > 0 && self.frames >= 1000 {
            return Err(MediaError::Capacity);
        }
        let flow = if self.used > 0 {
            let silence = match self.format {
                MediaFormat::Pcmu => 0xff,
                MediaFormat::Pcma => 0xd5,
                MediaFormat::Pcm8
                | MediaFormat::Pcm12
                | MediaFormat::Pcm16
                | MediaFormat::Pcm24
                | MediaFormat::Pcm32
                | MediaFormat::Pcm44
                | MediaFormat::Pcm48
                | MediaFormat::Opus => 0,
            };
            self.partial[self.used..usize::from(self.format.optimal_frame_size())].fill(silence);
            self.append_frame(self.decoded()?)
        } else {
            None
        };
        self.used = 0;
        self.mode = BufferMode::Discrete;
        self.queue
            .push_back(BufferItem::Notice(BufferNotice::BufferingCompleted(id)));
        self.markers += 1;
        Ok(flow)
    }

    pub fn pop(&mut self) -> BufferOutput {
        if self.playout == Playout::Paused {
            return BufferOutput {
                item: None,
                flow: None,
                drained: false,
            };
        }
        let item = self.queue.pop_front();
        match &item {
            Some(BufferItem::Frame(_)) => self.frames -= 1,
            Some(BufferItem::Notice(_)) => self.markers -= 1,
            None => {}
        }
        let flow = if self.flow == Flow::Backpressured && self.frames < 800 {
            self.flow = Flow::Open;
            Some(FlowNotice::Xon)
        } else {
            None
        };
        let drained = self.queue.is_empty() && self.drain == Drain::Requested;
        if drained {
            self.drain = Drain::Idle;
        }
        BufferOutput {
            item,
            flow,
            drained,
        }
    }

    pub fn pause(&mut self) {
        self.playout = Playout::Paused;
    }
    pub fn resume(&mut self) {
        self.playout = Playout::Running;
    }
    pub fn report_drained(&mut self) {
        self.drain = Drain::Requested;
    }
    pub const fn queued_frames(&self) -> usize {
        self.frames
    }
    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }
    pub fn is_buffering(&self) -> bool {
        self.mode == BufferMode::Continuous
    }
    pub fn is_paused(&self) -> bool {
        self.playout == Playout::Paused
    }
    pub fn is_full(&self) -> bool {
        self.flow == Flow::Backpressured
    }

    pub fn flush(&mut self) -> Option<FlowNotice> {
        self.queue.clear();
        self.frames = 0;
        self.markers = 0;
        self.used = 0;
        self.mode = BufferMode::Discrete;
        self.playout = Playout::Running;
        let flow = if self.flow == Flow::Backpressured {
            Some(FlowNotice::Xon)
        } else {
            None
        };
        self.flow = Flow::Open;
        flow
    }
}
