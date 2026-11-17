use super::frame;
use rusterisk_media::{AudioTarget, Frame, Gate, MediaError, Mixer};

#[test]
fn music_replaces_the_destination_mix_without_entering_other_channels_or_bridge_recordings(
) -> Result<(), MediaError> {
    let mut mixer = Mixer::new(2);
    mixer.insert(1)?;
    mixer.insert(2)?;
    mixer.bridge(10, &[1, 2])?;
    mixer.push(&1, frame(4000)?)?;
    mixer.push(&2, frame(-7000)?)?;
    let playback = [
        (AudioTarget::Bridge(10), frame(500)?),
        (AudioTarget::Channel(2), frame(-100)?),
    ];
    let music = [(1, Gate::Open, frame(1200)?)];
    let mixed = mixer.tick_with_music(&playback, &music)?;
    assert_eq!(mixed.channels[0].1.samples(), &[1200; 960]);
    assert_eq!(mixed.channels[1].1.samples(), &[4400; 960]);
    assert_eq!(mixed.bridges[0].1.samples(), &[-2500; 960]);
    Ok(())
}

#[test]
fn music_uses_its_purpose_gate_while_conversation_stays_closed() -> Result<(), MediaError> {
    for (gate, expected) in [
        (Gate::Open, 1200),
        (Gate::IncomingMuted, 1200),
        (Gate::OutgoingMuted, 0),
        (Gate::Closed, 0),
    ] {
        let mut mixer = Mixer::new(2);
        mixer.insert(1)?;
        mixer.insert(2)?;
        mixer.bridge(10, &[1, 2])?;
        mixer.gate(&1, Gate::Closed)?;
        mixer.push(&1, frame(4000)?)?;
        mixer.push(&2, frame(-7000)?)?;
        let mixed = mixer.tick_with_music(&[], &[(1, gate, frame(1200)?)])?;
        assert_eq!(mixed.channels[0].1.samples(), &[expected; 960]);
        assert_eq!(mixed.channels[1].1.samples(), &[0; 960]);
        assert_eq!(mixed.bridges[0].1.samples(), &[-7000; 960]);
    }
    Ok(())
}

#[test]
fn generated_permission_opens_channel_playback_without_opening_conversation_or_bridge_playback(
) -> Result<(), MediaError> {
    let mut mixer = Mixer::new(2);
    mixer.insert(1)?;
    mixer.insert(2)?;
    mixer.bridge(10, &[1, 2])?;
    let playback = [
        (AudioTarget::Channel(1), frame(3600)?),
        (AudioTarget::Bridge(10), frame(1000)?),
    ];
    for (generated, expected) in [(Gate::Open, 3600), (Gate::OutgoingMuted, 0)] {
        mixer.gates(&1, Gate::Closed, generated)?;
        mixer.push(&1, frame(4000)?)?;
        mixer.push(&2, frame(-7000)?)?;
        let mixed = mixer.tick_with_playback(&playback)?;
        assert_eq!(mixed.channels[0].1.samples(), &[expected; 960]);
        assert_eq!(mixed.channels[1].1.samples(), &[1000; 960]);
        assert_eq!(mixed.bridges[0].1.samples(), &[-6000; 960]);
    }
    mixer.gate(&1, Gate::Closed)?;
    let mixed = mixer.tick_with_playback(&playback)?;
    assert_eq!(mixed.channels[0].1.samples(), &[0; 960]);
    Ok(())
}

#[test]
fn an_active_muted_music_source_cannot_expose_bridge_audio() -> Result<(), MediaError> {
    let mut mixer = Mixer::new(2);
    mixer.insert(1)?;
    mixer.insert(2)?;
    mixer.bridge(10, &[1, 2])?;
    mixer.push(&2, frame(-7000)?)?;
    let mixed = mixer.tick_with_music(&[], &[(1, Gate::OutgoingMuted, frame(1200)?)])?;
    assert_eq!(mixed.channels[0].1.samples(), &[0; 960]);
    mixer.push(&2, frame(6000)?)?;
    let restored = mixer.tick_with_music(&[], &[])?;
    assert_eq!(restored.channels[0].1.samples(), &[6000; 960]);
    Ok(())
}

#[test]
fn music_admission_rejects_invalid_duplicate_and_unknown_sources_before_consuming_voice(
) -> Result<(), MediaError> {
    let mut mixer = Mixer::new(2);
    mixer.insert(1)?;
    mixer.insert(2)?;
    mixer.bridge(10, &[1, 2])?;
    mixer.push(&2, frame(-7000)?)?;
    for music in [
        [(1, Gate::Open, frame(1200)?), (1, Gate::Open, frame(3400)?)],
        [(1, Gate::Open, frame(1200)?), (3, Gate::Open, frame(3400)?)],
    ] {
        assert!(mixer.tick_with_music(&[], &music).is_err());
    }
    let short = Frame::new(&[1200; 240], 48000)?;
    assert!(matches!(
        mixer.tick_with_music(&[], &[(1, Gate::Open, short)]),
        Err(MediaError::InvalidFrame)
    ));
    let oversized = [(1, Gate::Open, frame(1200)?); 65];
    assert!(matches!(
        mixer.tick_with_music(&[], &oversized),
        Err(MediaError::Capacity)
    ));
    assert_eq!(mixer.tick()?[0].1.samples(), &[-7000; 960]);
    Ok(())
}
