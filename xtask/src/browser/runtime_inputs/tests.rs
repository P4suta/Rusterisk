use super::runtime_inputs::RuntimeInputs;
use std::{
    fs, io,
    net::{Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

pub(crate) fn wave(path: &Path, rate: u32, frequencies: &[f64]) -> io::Result<()> {
    let size = rate * 2;
    let mut bytes = Vec::with_capacity(44 + size as usize);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + size).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&rate.to_le_bytes());
    bytes.extend_from_slice(&(rate * 2).to_le_bytes());
    bytes.extend_from_slice(&2u16.to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&size.to_le_bytes());
    for index in 0..rate {
        let sample = frequencies
            .iter()
            .map(|frequency| {
                6000.0
                    * (std::f64::consts::TAU * frequency * f64::from(index) / f64::from(rate)).sin()
            })
            .sum::<f64>() as i16;
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    fs::write(path, bytes)
}

struct Fixture {
    root: PathBuf,
    inputs: RuntimeInputs,
}

fn using(test: impl FnOnce(&Fixture) -> io::Result<()>) -> io::Result<()> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    let root = fs::canonicalize(std::env::temp_dir())?.join(format!(
        "rusterisk-browser-inputs-{}-{stamp}",
        std::process::id()
    ));
    fs::create_dir(&root)?;
    let recordings = root.join("recordings");
    fs::create_dir(&recordings)?;
    let alice = recordings.join("alice-microphone.wav");
    let bob = recordings.join("bob-microphone.wav");
    let recording = recordings.join("browser-known.wav");
    wave(&alice, 48000, &[440.0])?;
    wave(&bob, 48000, &[660.0])?;
    wave(&recording, 8000, &[440.0, 660.0])?;
    let operation = RuntimeInputs::new(
        SocketAddr::from((Ipv4Addr::LOCALHOST, 48101)),
        SocketAddr::from((Ipv4Addr::LOCALHOST, 48102)),
        &alice,
        &bob,
        &recording,
    )
    .and_then(|inputs| {
        test(&Fixture {
            root: root.clone(),
            inputs,
        })
    });
    match (operation, fs::remove_dir_all(root)) {
        (operation, Ok(())) => operation,
        (Ok(()), Err(error)) => Err(error),
        (Err(operation), Err(cleanup)) => Err(io::Error::other(format!(
            "{operation}; browser runtime fixture cleanup failed: {cleanup}"
        ))),
    }
}

#[test]
fn retained_runtime_inputs_round_trip_without_reopening_original_audio() -> io::Result<()> {
    using(|fixture| {
        let saved = fixture.root.join("saved");
        fixture.inputs.save(&saved)?;
        fs::remove_dir_all(fixture.root.join("recordings"))?;
        let read = RuntimeInputs::read(&saved)?;
        assert_eq!(
            serde_json::to_value(read).map_err(io::Error::other)?,
            serde_json::to_value(&fixture.inputs).map_err(io::Error::other)?
        );
        Ok(())
    })
}

#[test]
fn retained_audio_rejects_silence_single_sender_and_corrupted_wave_headers() -> io::Result<()> {
    using(|fixture| {
        let saved = fixture.root.join("saved");
        fixture.inputs.save(&saved)?;
        let recording = saved.join("browser-known.wav");
        let valid = fs::read(&recording)?;
        for frequencies in [&[][..], &[440.0][..], &[660.0][..]] {
            wave(&recording, 8000, frequencies)?;
            assert!(RuntimeInputs::read(&saved).is_err());
        }
        let mut changed = valid.clone();
        changed[0] = b'X';
        fs::write(&recording, changed)?;
        assert!(RuntimeInputs::read(&saved).is_err());
        fs::write(&recording, valid)?;
        RuntimeInputs::read(&saved)?;
        Ok(())
    })
}

#[test]
fn retained_runtime_inputs_reject_extra_files_and_preserve_foreign_destinations() -> io::Result<()>
{
    using(|fixture| {
        let saved = fixture.root.join("saved");
        fixture.inputs.save(&saved)?;
        let foreign = saved.join("foreign");
        fs::write(&foreign, b"preserve")?;
        assert!(RuntimeInputs::read(&saved).is_err());
        assert!(fixture.inputs.save(&saved).is_err());
        assert_eq!(fs::read(foreign)?, b"preserve");
        Ok(())
    })
}

#[test]
fn runtime_inputs_reject_remote_addresses_and_ambiguous_original_paths() -> io::Result<()> {
    using(|fixture| {
        let recordings = fixture.root.join("recordings");
        let alice = recordings.join("alice-microphone.wav");
        let bob = recordings.join("bob-microphone.wav");
        let recording = recordings.join("browser-known.wav");
        assert!(RuntimeInputs::new(
            SocketAddr::from((Ipv4Addr::UNSPECIFIED, 48101)),
            SocketAddr::from((Ipv4Addr::LOCALHOST, 48102)),
            &alice,
            &bob,
            &recording,
        )
        .is_err());
        assert!(RuntimeInputs::new(
            SocketAddr::from((Ipv4Addr::LOCALHOST, 48101)),
            SocketAddr::from((Ipv4Addr::LOCALHOST, 48102)),
            Path::new("relative/alice-microphone.wav"),
            &bob,
            &recording,
        )
        .is_err());
        Ok(())
    })
}
