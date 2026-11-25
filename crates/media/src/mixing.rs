use crate::{Frame, MediaError, PcmFramer, MAX_SAMPLES};
use std::collections::{BTreeMap, BTreeSet};

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
    frames: PcmFramer,
    mix_rate: u32,
    gate: Gate,
    generated_audio_gate: Gate,
}

struct BridgeSum {
    id: u64,
    rate: u32,
    samples: [i32; MAX_SAMPLES],
}

pub struct Mixer<K> {
    capacity: usize,
    inputs: BTreeMap<K, Input>,
    // A tick reuses this storage, so a steady set of members allocates nothing.
    heard: Vec<Option<Frame>>,
    sums: Vec<BridgeSum>,
    order: Vec<usize>,
    mixed: MixedAudio<K>,
    // The frame passes of the last tick, which a test bounds by the member count.
    #[cfg(test)]
    passes: usize,
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
            heard: Vec::new(),
            sums: Vec::new(),
            order: Vec::new(),
            mixed: MixedAudio {
                channels: Vec::new(),
                bridges: Vec::new(),
            },
            #[cfg(test)]
            passes: 0,
        }
    }

    pub fn insert(&mut self, key: K) -> Result<(), MediaError> {
        self.insert_at_rate(key, 48000)
    }

    pub fn insert_at_rate(&mut self, key: K, rate: u32) -> Result<(), MediaError> {
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
                frames: PcmFramer::with_capacity(rate, 3)?,
                mix_rate: rate,
                gate: Gate::Open,
                generated_audio_gate: Gate::Open,
            },
        );
        Ok(())
    }

    pub fn remove(&mut self, key: &K) -> bool {
        let Some(input) = self.inputs.remove(key) else {
            return false;
        };
        if let Some(bridge) = input.bridge {
            self.update_rate(bridge);
        }
        true
    }

    fn update_rate(&mut self, bridge: u64) {
        let members = |input: &&Input| input.bridge == Some(bridge);
        if let Some(rate) = self
            .inputs
            .values()
            .filter(members)
            .map(|input| input.frames.rate())
            .max()
        {
            for input in self
                .inputs
                .values_mut()
                .filter(|input| input.bridge == Some(bridge))
            {
                input.mix_rate = rate;
            }
        }
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
                input.mix_rate = input.frames.rate();
            }
        }
        self.update_rate(id);
        Ok(())
    }

    pub fn gate(&mut self, key: &K, gate: Gate) -> Result<(), MediaError> {
        self.gates(key, gate, gate)
    }

    pub fn gates(
        &mut self,
        key: &K,
        gate: Gate,
        generated_audio_gate: Gate,
    ) -> Result<(), MediaError> {
        let input = self.inputs.get_mut(key).ok_or(MediaError::UnknownStream)?;
        if input.gate != gate {
            input.frames.clear();
            input.gate = gate;
        }
        input.generated_audio_gate = generated_audio_gate;
        Ok(())
    }

    pub fn push(&mut self, key: &K, frame: Frame) -> Result<(), MediaError> {
        let input = self.inputs.get_mut(key).ok_or(MediaError::UnknownStream)?;
        if frame.samples().len() * 50 != frame.rate() as usize {
            return Err(MediaError::InvalidFrame);
        }
        input.frames.push(frame.resample(input.frames.rate())?)
    }

    pub fn tick(&mut self) -> Result<Vec<(K, Frame)>, MediaError> {
        Ok(self.tick_with_recordings()?.channels.clone())
    }

    pub fn tick_with_recordings(&mut self) -> Result<&MixedAudio<K>, MediaError> {
        self.tick_with_playback(&[])
    }

    pub fn tick_with_playback(
        &mut self,
        playback: &[(AudioTarget<K>, Frame)],
    ) -> Result<&MixedAudio<K>, MediaError> {
        self.tick_with_music(playback, &[])
    }

    // The result stays valid until the next tick.
    pub fn tick_with_music(
        &mut self,
        playback: &[(AudioTarget<K>, Frame)],
        music: &[(K, Gate, Frame)],
    ) -> Result<&MixedAudio<K>, MediaError> {
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
        if music.iter().enumerate().any(|(index, (key, _, _))| {
            music
                .iter()
                .skip(index + 1)
                .any(|(other, _, _)| other == key)
        }) {
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
        let Self {
            inputs,
            heard,
            sums,
            order,
            mixed,
            ..
        } = self;
        // Each bridge adds every received voice once.
        heard.clear();
        sums.clear();
        #[cfg(test)]
        let mut passes = 0_usize;
        for input in inputs.values_mut() {
            let frame = input
                .frames
                .pop()
                .filter(|_| input.gate.receives())
                .map(|frame| frame.resample(input.mix_rate))
                .transpose()?;
            if let Some(bridge) = input.bridge {
                let sum = bridge_sum(sums, bridge, input.mix_rate);
                if let Some(frame) = &frame {
                    add(&mut sum.samples, frame);
                    #[cfg(test)]
                    {
                        passes += 1;
                    }
                }
            }
            heard.push(frame);
        }
        for (target, _) in playback {
            match target {
                AudioTarget::Bridge(bridge) => {
                    bridge_sum(sums, *bridge, 48000);
                }
                AudioTarget::Channel(_) => {}
            }
        }
        // The stored keys change only when the members change.
        if !mixed.channels.iter().map(|(key, _)| key).eq(inputs.keys()) {
            let silence = Frame::new(&[0; MAX_SAMPLES], 48000)?;
            mixed.channels.clear();
            mixed
                .channels
                .extend(inputs.keys().map(|key| (key.clone(), silence)));
        }
        for (((key, input), heard), (_, output)) in
            inputs.iter().zip(heard.iter()).zip(&mut mixed.channels)
        {
            let mut sum = [0i32; MAX_SAMPLES];
            if let Some((_, generated_gate, frame)) =
                music.iter().find(|(target, _, _)| target == key)
            {
                if generated_gate.sends() {
                    add(&mut sum, &frame.resample(input.mix_rate)?);
                }
            } else {
                // A member hears its bridge without its own voice.
                if let Some(bridge) = input.bridge.filter(|_| input.gate.sends()) {
                    if let Some(total) = sums.iter().find(|total| total.id == bridge) {
                        sum = total.samples;
                    }
                    if let Some(own) = heard {
                        for (sum, sample) in sum.iter_mut().zip(own.samples()) {
                            *sum -= i32::from(*sample);
                        }
                        #[cfg(test)]
                        {
                            passes += 1;
                        }
                    }
                }
                for (target, frame) in playback {
                    let permitted = match target {
                        AudioTarget::Channel(destination) => {
                            destination == key && input.generated_audio_gate.sends()
                        }
                        AudioTarget::Bridge(destination) => {
                            Some(*destination) == input.bridge && input.gate.sends()
                        }
                    };
                    if permitted {
                        add(&mut sum, &frame.resample(input.mix_rate)?);
                    }
                }
            }
            *output = clamped(&sum, input.mix_rate)?;
        }
        order.clear();
        order.extend(0..sums.len());
        order.sort_unstable_by_key(|index| sums[*index].id);
        mixed.bridges.clear();
        for index in order.iter() {
            let bridge = &mut sums[*index];
            for (target, frame) in playback {
                if *target == AudioTarget::Bridge(bridge.id) {
                    add(&mut bridge.samples, &frame.resample(bridge.rate)?);
                }
            }
            mixed
                .bridges
                .push((bridge.id, clamped(&bridge.samples, bridge.rate)?));
        }
        #[cfg(test)]
        {
            self.passes = passes;
        }
        Ok(&self.mixed)
    }
}

fn add(sum: &mut [i32; MAX_SAMPLES], frame: &Frame) {
    for (sum, sample) in sum.iter_mut().zip(frame.samples()) {
        *sum += i32::from(*sample);
    }
}

fn clamped(sum: &[i32; MAX_SAMPLES], rate: u32) -> Result<Frame, MediaError> {
    let len = rate as usize / 50;
    let mut samples = [0; MAX_SAMPLES];
    for (target, source) in samples[..len].iter_mut().zip(sum) {
        *target = (*source).clamp(i16::MIN.into(), i16::MAX.into()) as i16;
    }
    Frame::new(&samples[..len], rate)
}

// The running sum of one bridge, which starts at silence.
fn bridge_sum(sums: &mut Vec<BridgeSum>, id: u64, rate: u32) -> &mut BridgeSum {
    if let Some(index) = sums.iter().position(|sum| sum.id == id) {
        return &mut sums[index];
    }
    let index = sums.len();
    sums.push(BridgeSum {
        id,
        rate,
        samples: [0; MAX_SAMPLES],
    });
    &mut sums[index]
}

#[cfg(test)]
mod tests {
    use super::{Frame, MediaError, Mixer};

    #[test]
    fn a_steady_set_of_members_reuses_the_stored_result() -> Result<(), MediaError> {
        let mut mixer = Mixer::new(8);
        let keys: Vec<u32> = (0..4).collect();
        for key in &keys {
            mixer.insert(*key)?;
        }
        mixer.bridge(1, &keys)?;
        let mut stored = Vec::with_capacity(3);
        for _ in 0..3 {
            for key in &keys {
                mixer.push(key, Frame::new(&[100; 960], 48000)?)?;
            }
            let mixed = mixer.tick_with_music(&[], &[])?;
            stored.push((mixed.channels.as_ptr(), mixed.bridges.as_ptr()));
            assert_eq!(mixed.channels.len(), 4);
        }
        assert_eq!(stored[1], stored[2]);
        let capacities = (
            mixer.heard.capacity(),
            mixer.sums.capacity(),
            mixer.order.capacity(),
        );
        mixer.tick_with_music(&[], &[])?;
        assert_eq!(
            capacities,
            (
                mixer.heard.capacity(),
                mixer.sums.capacity(),
                mixer.order.capacity()
            )
        );
        mixer.insert(9)?;
        assert_eq!(mixer.tick_with_music(&[], &[])?.channels.len(), 5);
        Ok(())
    }

    #[test]
    fn the_frame_passes_of_a_bridge_tick_grow_with_its_member_count() -> Result<(), MediaError> {
        for members in [2_u32, 8, 64] {
            let mut mixer = Mixer::new(64);
            let keys: Vec<u32> = (0..members).collect();
            for key in &keys {
                mixer.insert(*key)?;
            }
            mixer.bridge(1, &keys)?;
            for key in &keys {
                mixer.push(key, Frame::new(&[100; 960], 48000)?)?;
            }
            mixer.passes = 0;
            mixer.tick()?;
            assert!(
                mixer.passes <= 2 * members as usize,
                "{members} members took {} frame passes",
                mixer.passes
            );
        }
        Ok(())
    }
}
