use crate::{Frame, MediaError, MAX_SAMPLES};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gate {
    Open,
    IncomingMuted,
    OutgoingMuted,
    Closed,
}

impl Gate {
    const fn receives(self) -> bool {
        match self {
            Self::Open | Self::OutgoingMuted => true,
            Self::IncomingMuted | Self::Closed => false,
        }
    }
    const fn sends(self) -> bool {
        match self {
            Self::Open | Self::IncomingMuted => true,
            Self::OutgoingMuted | Self::Closed => false,
        }
    }
}

struct Input {
    bridge: Option<u64>,
    frames: VecDeque<Frame>,
    gate: Gate,
}

pub struct Mixer<K> {
    capacity: usize,
    inputs: BTreeMap<K, Input>,
}

impl<K: Ord + Clone> Mixer<K> {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.min(64),
            inputs: BTreeMap::new(),
        }
    }

    pub fn insert(&mut self, key: K) -> Result<(), MediaError> {
        if self.inputs.contains_key(&key) {
            return Err(MediaError::InvalidRoute);
        }
        if self.inputs.len() >= self.capacity {
            return Err(MediaError::Capacity);
        }
        self.inputs.insert(
            key,
            Input {
                bridge: None,
                frames: VecDeque::with_capacity(3),
                gate: Gate::Open,
            },
        );
        Ok(())
    }

    pub fn remove(&mut self, key: &K) -> bool {
        self.inputs.remove(key).is_some()
    }

    pub fn bridge(&mut self, id: u64, members: &[K]) -> Result<(), MediaError> {
        if members.len() > self.capacity
            || members.iter().collect::<BTreeSet<_>>().len() != members.len()
        {
            return Err(MediaError::InvalidRoute);
        }
        for key in members {
            let input = self.inputs.get(key).ok_or(MediaError::UnknownStream)?;
            if input.bridge.is_some_and(|bridge| bridge != id) {
                return Err(MediaError::InvalidRoute);
            }
        }
        for (key, input) in &mut self.inputs {
            let bridge = if members.contains(key) {
                Some(id)
            } else if input.bridge == Some(id) {
                None
            } else {
                input.bridge
            };
            if input.bridge != bridge {
                input.frames.clear();
                input.bridge = bridge;
            }
        }
        Ok(())
    }

    pub fn gate(&mut self, key: &K, gate: Gate) -> Result<(), MediaError> {
        let input = self.inputs.get_mut(key).ok_or(MediaError::UnknownStream)?;
        if input.gate != gate {
            input.frames.clear();
            input.gate = gate;
        }
        Ok(())
    }

    pub fn push(&mut self, key: &K, frame: Frame) -> Result<(), MediaError> {
        let input = self.inputs.get_mut(key).ok_or(MediaError::UnknownStream)?;
        if input.frames.len() >= 3 {
            return Err(MediaError::Capacity);
        }
        let frame = frame.resample(48000)?;
        if frame.samples().len() != MAX_SAMPLES {
            return Err(MediaError::InvalidFrame);
        }
        input.frames.push_back(frame);
        Ok(())
    }

    pub fn tick(&mut self) -> Result<Vec<(K, Frame)>, MediaError> {
        let frames: Vec<_> = self
            .inputs
            .iter_mut()
            .map(|(key, input)| {
                let frame = input.frames.pop_front();
                (
                    key.clone(),
                    input.bridge,
                    input.gate,
                    frame.filter(|_| input.gate.receives()),
                )
            })
            .collect();
        let mut output = Vec::with_capacity(frames.len());
        for (key, bridge, gate, _) in &frames {
            let mut sum = [0i32; MAX_SAMPLES];
            if bridge.is_some() && gate.sends() {
                for (source, source_bridge, _, frame) in &frames {
                    if source != key && source_bridge == bridge {
                        if let Some(frame) = frame {
                            for (sum, sample) in sum.iter_mut().zip(frame.samples()) {
                                *sum += i32::from(*sample);
                            }
                        }
                    }
                }
            }
            let samples = sum.map(|sample| sample.clamp(i16::MIN.into(), i16::MAX.into()) as i16);
            output.push((key.clone(), Frame::new(&samples, 48000)?));
        }
        Ok(output)
    }
}
