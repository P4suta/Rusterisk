use rusterisk_media::{AudioTarget, Frame, Gate, MediaError, Mixer, MAX_SAMPLES};

// The mixing rule as each member adding every other member, which the mixer must reproduce sample for sample.
struct Member {
    key: u32,
    bridge: Option<u64>,
    gate: Gate,
    generated: Gate,
    frame: Option<Frame>,
}

fn receives(gate: Gate) -> bool {
    matches!(gate, Gate::Open | Gate::OutgoingMuted)
}

fn sends(gate: Gate) -> bool {
    matches!(gate, Gate::Open | Gate::IncomingMuted)
}

fn add(sum: &mut [i32; MAX_SAMPLES], frame: &Frame) {
    for (sum, sample) in sum.iter_mut().zip(frame.samples()) {
        *sum += i32::from(*sample);
    }
}

fn clamped(sum: [i32; MAX_SAMPLES]) -> Result<Frame, MediaError> {
    Frame::new(
        &sum.map(|sample| sample.clamp(i16::MIN.into(), i16::MAX.into()) as i16),
        48000,
    )
}

type Mixed = (Vec<(u32, Frame)>, Vec<(u64, Frame)>);

fn reference(
    members: &[Member],
    playback: &[(AudioTarget<u32>, Frame)],
    music: &[(u32, Gate, Frame)],
) -> Result<Mixed, MediaError> {
    let heard = |member: &Member| member.frame.filter(|_| receives(member.gate));
    let mut channels = Vec::with_capacity(members.len());
    for member in members {
        let mut sum = [0; MAX_SAMPLES];
        if let Some((_, gate, frame)) = music.iter().find(|(key, _, _)| *key == member.key) {
            if sends(*gate) {
                add(&mut sum, frame);
            }
        } else {
            if member.bridge.is_some() && sends(member.gate) {
                for source in members {
                    if source.key != member.key && source.bridge == member.bridge {
                        if let Some(frame) = heard(source) {
                            add(&mut sum, &frame);
                        }
                    }
                }
            }
            for (target, frame) in playback {
                let permitted = match target {
                    AudioTarget::Channel(key) => *key == member.key && sends(member.generated),
                    AudioTarget::Bridge(bridge) => {
                        Some(*bridge) == member.bridge && sends(member.gate)
                    }
                };
                if permitted {
                    add(&mut sum, frame);
                }
            }
        }
        channels.push((member.key, clamped(sum)?));
    }
    let mut identifiers: Vec<u64> = members
        .iter()
        .filter_map(|member| member.bridge)
        .chain(playback.iter().filter_map(|(target, _)| match target {
            AudioTarget::Bridge(bridge) => Some(*bridge),
            AudioTarget::Channel(_) => None,
        }))
        .collect();
    identifiers.sort_unstable();
    identifiers.dedup();
    let mut bridges = Vec::with_capacity(identifiers.len());
    for bridge in identifiers {
        let mut sum = [0; MAX_SAMPLES];
        for member in members {
            if member.bridge == Some(bridge) {
                if let Some(frame) = heard(member) {
                    add(&mut sum, &frame);
                }
            }
        }
        for (target, frame) in playback {
            if *target == AudioTarget::Bridge(bridge) {
                add(&mut sum, frame);
            }
        }
        bridges.push((bridge, clamped(sum)?));
    }
    Ok((channels, bridges))
}

// A fixed sequence, so that every run compares the same scenarios.
struct Sequence(u64);

impl Sequence {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }

    fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound
    }

    fn gate(&mut self) -> Gate {
        match self.below(6) {
            0 => Gate::IncomingMuted,
            1 => Gate::OutgoingMuted,
            2 => Gate::Closed,
            _ => Gate::Open,
        }
    }

    // Loud frames make the sums of large bridges pass the clipping limits.
    fn frame(&mut self) -> Result<Frame, MediaError> {
        let loud = self.below(4) == 0;
        let mut samples = [0_i16; MAX_SAMPLES];
        for sample in &mut samples {
            let value = self.below(65_536) as i32 - 32_768;
            *sample = if loud {
                if value < 0 {
                    i16::MIN
                } else {
                    i16::MAX
                }
            } else {
                value as i16
            };
        }
        Frame::new(&samples, 48000)
    }
}

fn scenario(seed: u64) -> Result<(), MediaError> {
    let mut sequence = Sequence(seed);
    let count = 1 + sequence.below(64) as u32;
    let bridges = 1 + sequence.below(4);
    let mut mixer = Mixer::new(64);
    let mut members = Vec::with_capacity(count as usize);
    for key in 0..count {
        mixer.insert(key)?;
        let bridge = (sequence.below(5) != 0).then(|| 100 + sequence.below(bridges));
        members.push(Member {
            key,
            bridge,
            gate: Gate::Open,
            generated: Gate::Open,
            frame: None,
        });
    }
    for bridge in 100..100 + bridges {
        let joined: Vec<u32> = members
            .iter()
            .filter(|member| member.bridge == Some(bridge))
            .map(|member| member.key)
            .collect();
        mixer.bridge(bridge, &joined)?;
    }
    for round in 0..3 {
        for member in &mut members {
            member.gate = sequence.gate();
            member.generated = sequence.gate();
            mixer.gates(&member.key, member.gate, member.generated)?;
            member.frame = if sequence.below(4) == 0 {
                None
            } else {
                let frame = sequence.frame()?;
                mixer.push(&member.key, frame)?;
                Some(frame)
            };
        }
        let mut playback = Vec::new();
        for _ in 0..sequence.below(4) {
            let target = if sequence.below(2) == 0 {
                AudioTarget::Channel(sequence.below(u64::from(count)) as u32)
            } else {
                AudioTarget::Bridge(100 + sequence.below(bridges + 1))
            };
            playback.push((target, sequence.frame()?));
        }
        let mut music: Vec<(u32, Gate, Frame)> = Vec::new();
        for _ in 0..sequence.below(3) {
            let key = sequence.below(u64::from(count)) as u32;
            if music.iter().all(|(taken, _, _)| *taken != key) {
                music.push((key, sequence.gate(), sequence.frame()?));
            }
        }
        let (channels, bridges) = reference(&members, &playback, &music)?;
        let mixed = mixer.tick_with_music(&playback, &music)?;
        assert_eq!(mixed.channels, channels, "seed {seed} round {round}");
        assert_eq!(mixed.bridges, bridges, "seed {seed} round {round}");
    }
    Ok(())
}

#[test]
fn every_output_sample_equals_the_sum_of_the_other_members() -> Result<(), MediaError> {
    for seed in 1..=200 {
        scenario(seed)?;
    }
    Ok(())
}
