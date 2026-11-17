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

pub struct MixedAudio<K> {
    pub channels: Vec<(K, Frame)>,
    pub bridges: Vec<(u64, Frame)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AudioTarget<K> {
    Channel(K),
    Bridge(u64),
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

    pub fn bridge_for(&self, key: &K) -> Option<u64> {
        self.inputs.get(key).and_then(|input| input.bridge)
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
        Ok(self.tick_with_recordings()?.channels)
    }

    pub fn tick_with_recordings(&mut self) -> Result<MixedAudio<K>, MediaError> {
        self.tick_with_playback(&[])
    }

    pub fn tick_with_playback(
        &mut self,
        playback: &[(AudioTarget<K>, Frame)],
    ) -> Result<MixedAudio<K>, MediaError> {
        self.tick_with_music(playback, &[])
    }

    pub fn tick_with_music(
        &mut self,
        playback: &[(AudioTarget<K>, Frame)],
        music: &[(K, Gate, Frame)],
    ) -> Result<MixedAudio<K>, MediaError> {
        if playback
            .len()
            .checked_add(music.len())
            .filter(|count| *count <= 64)
            .is_none()
        {
            return Err(MediaError::Capacity);
        }
        for (target, frame) in playback {
            if frame.rate() != 48000 || frame.samples().len() != MAX_SAMPLES {
                return Err(MediaError::InvalidFrame);
            }
            match target {
                AudioTarget::Channel(key) if !self.inputs.contains_key(key) => {
                    return Err(MediaError::UnknownStream)
                }
                AudioTarget::Channel(_) | AudioTarget::Bridge(_) => {}
            }
        }
        if music
            .iter()
            .map(|(key, _, _)| key)
            .collect::<BTreeSet<_>>()
            .len()
            != music.len()
        {
            return Err(MediaError::InvalidRoute);
        }
        for (key, _, frame) in music {
            if frame.rate() != 48000 || frame.samples().len() != MAX_SAMPLES {
                return Err(MediaError::InvalidFrame);
            }
            if !self.inputs.contains_key(key) {
                return Err(MediaError::UnknownStream);
            }
        }
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
            if let Some((_, music_gate, frame)) = music.iter().find(|(target, _, _)| target == key)
            {
                if music_gate.sends() {
                    for (sum, sample) in sum.iter_mut().zip(frame.samples()) {
                        *sum = i32::from(*sample);
                    }
                }
            } else {
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
                if gate.sends() {
                    for (target, frame) in playback {
                        let matches = match target {
                            AudioTarget::Channel(destination) => destination == key,
                            AudioTarget::Bridge(destination) => Some(*destination) == *bridge,
                        };
                        if matches {
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
        let bridge_ids = frames
            .iter()
            .filter_map(|(_, bridge, _, _)| *bridge)
            .chain(playback.iter().filter_map(|(target, _)| match target {
                AudioTarget::Bridge(bridge) => Some(*bridge),
                AudioTarget::Channel(_) => None,
            }))
            .collect::<BTreeSet<_>>();
        let mut bridges = Vec::with_capacity(bridge_ids.len());
        for bridge in bridge_ids {
            let mut sum = [0i32; MAX_SAMPLES];
            for (_, source_bridge, _, frame) in &frames {
                if *source_bridge == Some(bridge) {
                    if let Some(frame) = frame {
                        for (sum, sample) in sum.iter_mut().zip(frame.samples()) {
                            *sum += i32::from(*sample);
                        }
                    }
                }
            }
            for (target, frame) in playback {
                if *target == AudioTarget::Bridge(bridge) {
                    for (sum, sample) in sum.iter_mut().zip(frame.samples()) {
                        *sum += i32::from(*sample);
                    }
                }
            }
            let samples = sum.map(|sample| sample.clamp(i16::MIN.into(), i16::MAX.into()) as i16);
            bridges.push((bridge, Frame::new(&samples, 48000)?));
        }
        Ok(MixedAudio {
            channels: output,
            bridges,
        })
    }
}
