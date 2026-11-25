use rusterisk_media::{AudioTarget, Frame, Gate, MediaError, Mixer};

#[path = "mixing/music.rs"]
mod music;
#[path = "mixing/reference.rs"]
mod reference;

fn frame(value: i16) -> Result<Frame, MediaError> {
    Frame::new(&[value; 960], 48000)
}

#[test]
fn a_bridge_uses_its_highest_negotiated_rate_even_when_that_member_is_silent(
) -> Result<(), MediaError> {
    let mut mixer = Mixer::new(3);
    for (key, rate) in [(1, 8000), (2, 12000), (3, 44100)] {
        mixer.insert_at_rate(key, rate)?;
    }
    mixer.bridge(1, &[1, 2, 3])?;
    let samples = std::array::from_fn::<_, 160, _>(|index| (index as i16 - 80) * 127);
    let voice = Frame::new(&samples, 8000)?;
    for (leaving, rate) in [(None, 44100), (Some(3), 12000), (Some(2), 8000)] {
        if let Some(leaving) = leaving {
            assert!(mixer.remove(&leaving));
        }
        mixer.push(&1, voice)?;
        let mixed = mixer.tick_with_recordings()?;
        assert_eq!(mixed.bridges, [(1, voice.resample(rate)?)]);
        assert_eq!(mixed.channels[0].1.rate(), rate);
        assert!(mixed.channels[0]
            .1
            .samples()
            .iter()
            .all(|value| *value == 0));
        if let Some((_, peer)) = mixed.channels.get(1) {
            assert_eq!(*peer, voice.resample(rate)?);
        }
    }
    Ok(())
}

#[test]
fn bridges_mix_received_samples_without_sending_a_call_its_own_audio() -> Result<(), MediaError> {
    let mut mixer = Mixer::new(4);
    for key in 1..=4 {
        mixer.insert(key)?;
    }
    mixer.bridge(10, &[1, 2, 3])?;
    for (key, sample) in [(1, 1000), (2, 2000), (3, -3000), (4, 15000)] {
        mixer.push(&key, frame(sample)?)?;
    }
    let output = mixer.tick()?;
    for (key, expected) in [(1, -1000), (2, -2000), (3, 3000), (4, 0)] {
        let (_, audio) = output
            .iter()
            .find(|(value, _)| *value == key)
            .ok_or(MediaError::InvalidFrame)?;
        assert!(audio.samples().iter().all(|sample| *sample == expected));
    }
    Ok(())
}

#[test]
fn clipping_happens_after_all_contributions_and_muting_preserves_direction(
) -> Result<(), MediaError> {
    let mut mixer = Mixer::new(4);
    for key in 1..=4 {
        mixer.insert(key)?;
    }
    mixer.bridge(1, &[1, 2, 3, 4])?;
    for (key, sample) in [(1, 30000), (2, 30000), (3, -30000)] {
        mixer.push(&key, frame(sample)?)?;
    }
    assert_eq!(mixer.tick()?[3].1.samples()[0], 30000);
    mixer.gate(&1, Gate::IncomingMuted)?;
    mixer.gate(&2, Gate::OutgoingMuted)?;
    mixer.push(&1, frame(1000)?)?;
    mixer.push(&2, frame(2000)?)?;
    let output = mixer.tick()?;
    assert_eq!(output[0].1.samples()[0], 2000);
    assert_eq!(output[1].1.samples()[0], 0);
    assert_eq!(output[2].1.samples()[0], 2000);
    Ok(())
}

#[test]
fn capacity_and_membership_failures_preserve_existing_routes() -> Result<(), MediaError> {
    let mut mixer = Mixer::new(2);
    mixer.insert(1)?;
    mixer.insert(2)?;
    assert!(matches!(mixer.insert(3), Err(MediaError::Capacity)));
    mixer.bridge(1, &[1, 2])?;
    assert!(mixer.bridge(2, &[1, 3]).is_err());
    mixer.push(&1, frame(1000)?)?;
    mixer.bridge(1, &[2])?;
    assert!(mixer
        .tick()?
        .iter()
        .all(|(_, audio)| audio.samples().iter().all(|value| *value == 0)));
    for _ in 0..3 {
        mixer.push(&1, frame(1000)?)?;
    }
    assert!(matches!(
        mixer.push(&1, frame(1000)?),
        Err(MediaError::Capacity)
    ));
    assert!(mixer.remove(&1));
    assert!(!mixer.remove(&1));
    assert!(mixer.push(&1, frame(1000)?).is_err());
    Ok(())
}

#[test]
fn bridge_recordings_include_every_received_voice_once_and_respect_input_gates(
) -> Result<(), MediaError> {
    let mut mixer = Mixer::new(4);
    for key in 1..=4 {
        mixer.insert(key)?;
    }
    mixer.bridge(10, &[1, 2, 3])?;
    mixer.bridge(20, &[4])?;
    for (key, sample) in [(1, 30000), (2, 30000), (3, -30000), (4, -1200)] {
        mixer.push(&key, frame(sample)?)?;
    }
    let output = mixer.tick_with_recordings()?;
    assert_eq!(output.bridges.len(), 2);
    assert_eq!(output.bridges[0].0, 10);
    assert_eq!(output.bridges[0].1.samples(), &[30000; 960]);
    assert_eq!(output.bridges[1].1.samples(), &[-1200; 960]);
    assert_eq!(output.channels[3].1.samples(), &[0; 960]);
    mixer.gate(&1, Gate::IncomingMuted)?;
    mixer.gate(&2, Gate::OutgoingMuted)?;
    mixer.push(&1, frame(1000)?)?;
    mixer.push(&2, frame(2000)?)?;
    let output = mixer.tick_with_recordings()?;
    assert_eq!(output.bridges[0].1.samples(), &[2000; 960]);
    assert_eq!(output.channels[1].1.samples(), &[0; 960]);
    Ok(())
}

#[test]
fn playback_targets_add_audio_before_clipping_and_channel_audio_stays_out_of_bridge_recordings(
) -> Result<(), MediaError> {
    let mut mixer = Mixer::new(4);
    for key in 1..=4 {
        mixer.insert(key)?;
    }
    mixer.bridge(10, &[1, 2, 3])?;
    for (key, sample) in [(1, 30000), (2, 30000), (3, -30000)] {
        mixer.push(&key, frame(sample)?)?;
    }
    let playback = [
        (AudioTarget::Bridge(10), frame(-30000)?),
        (AudioTarget::Channel(4), frame(800)?),
    ];
    let output = mixer.tick_with_playback(&playback)?;
    assert_eq!(output.bridges[0].1.samples(), &[0; 960]);
    assert_eq!(output.channels[2].1.samples(), &[30000; 960]);
    assert_eq!(output.channels[3].1.samples(), &[800; 960]);
    mixer.gate(&3, Gate::OutgoingMuted)?;
    let output = mixer.tick_with_playback(&playback)?;
    assert_eq!(output.channels[2].1.samples(), &[0; 960]);
    assert_eq!(output.bridges[0].1.samples(), &[-30000; 960]);
    Ok(())
}
