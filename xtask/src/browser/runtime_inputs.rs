use serde::Deserialize;
#[cfg(test)]
use serde::Serialize;
use std::{
    collections::BTreeSet,
    fs,
    io::{self, Read},
    net::SocketAddr,
    path::{Path, PathBuf},
};

const FILES: [&str; 4] = [
    "runtime-inputs.json",
    "alice-microphone.wav",
    "bob-microphone.wav",
    "browser-known.wav",
];
const MAX_AUDIO: u64 = 4 * 1024 * 1024;

#[cfg(test)]
use std::io::Write;

#[derive(Deserialize)]
#[cfg_attr(test, derive(Serialize))]
#[serde(deny_unknown_fields)]
pub(crate) struct RuntimeInputs {
    format: u32,
    ui: SocketAddr,
    server: SocketAddr,
    alice: PathBuf,
    bob: PathBuf,
    recording: PathBuf,
}

fn invalid(reason: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, reason)
}

fn original(path: &Path, file: &str) -> io::Result<String> {
    let text = path
        .to_str()
        .ok_or_else(|| invalid("browser runtime input path is not UTF-8"))?;
    if text.len() > 4096 || text.chars().any(char::is_control) {
        return Err(invalid("browser runtime input path exceeds its profile"));
    }
    let normalized = text.replace('\\', "/");
    let absolute = normalized.starts_with('/')
        || (normalized
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphabetic)
            && normalized.as_bytes().get(1..3) == Some(b":/"));
    if !absolute || normalized.split('/').any(|part| matches!(part, "." | "..")) {
        return Err(invalid(
            "browser runtime input path is not absolute and normalized",
        ));
    }
    let (parent, name) = normalized
        .rsplit_once('/')
        .ok_or_else(|| invalid("browser runtime input has no filename"))?;
    if name != file || parent.rsplit('/').next() != Some("recordings") {
        return Err(invalid(
            "browser runtime audio path differs from its native role",
        ));
    }
    Ok(parent.to_owned())
}

fn read(path: &Path, limit: u64) -> io::Result<Vec<u8>> {
    let before = fs::symlink_metadata(path)?;
    if !before.is_file() || before.len() > limit {
        return Err(invalid(
            "browser runtime output is not a bounded regular file",
        ));
    }
    let mut file = fs::File::open(path)?;
    let opened = file.metadata()?;
    if !opened.is_file()
        || opened.len() != before.len()
        || opened.modified()? != before.modified()?
    {
        return Err(invalid("browser runtime output changed before reading"));
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(limit + 1)
        .read_to_end(&mut bytes)?;
    let after = file.metadata()?;
    if bytes.len() as u64 != opened.len()
        || after.len() != opened.len()
        || after.modified()? != opened.modified()?
    {
        return Err(invalid("browser runtime output changed while reading"));
    }
    Ok(bytes)
}

#[cfg(test)]
fn write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(bytes)?;
    file.flush()
}

fn pcm(bytes: &[u8], rate: u32) -> io::Result<&[u8]> {
    if bytes.len() < 46 || bytes.len() as u64 > MAX_AUDIO {
        return Err(invalid("browser audio requires bounded nonempty PCM data"));
    }
    let size = u32::try_from(bytes.len() - 44).map_err(io::Error::other)?;
    if size % 2 != 0
        || &bytes[..4] != b"RIFF"
        || bytes[4..8] != (size + 36).to_le_bytes()
        || &bytes[8..16] != b"WAVEfmt "
        || bytes[16..20] != 16u32.to_le_bytes()
        || bytes[20..22] != 1u16.to_le_bytes()
        || bytes[22..24] != 1u16.to_le_bytes()
        || bytes[24..28] != rate.to_le_bytes()
        || bytes[28..32] != (rate * 2).to_le_bytes()
        || bytes[32..34] != 2u16.to_le_bytes()
        || bytes[34..36] != 16u16.to_le_bytes()
        || &bytes[36..40] != b"data"
        || bytes[40..44] != size.to_le_bytes()
    {
        return Err(invalid(
            "browser audio differs from the native mono PCM format",
        ));
    }
    Ok(&bytes[44..])
}

fn matching(bytes: &[u8], rate: u32) -> io::Result<[usize; 2]> {
    let samples = pcm(bytes, rate)?;
    let frame_bytes = (rate / 50 * 2) as usize;
    let mut frames = samples.chunks_exact(frame_bytes);
    if !frames.remainder().is_empty() || frames.len() > 500 {
        return Err(invalid("browser audio exceeds its bounded frame profile"));
    }
    let mut matching = [0; 2];
    for frame in &mut frames {
        let length = frame.len() / 2;
        let samples = || {
            frame
                .as_chunks::<2>()
                .0
                .iter()
                .map(|pair| f64::from(i16::from_le_bytes([pair[0], pair[1]])))
        };
        let rms = (samples().map(|sample| sample * sample).sum::<f64>() / length as f64).sqrt();
        for (count, frequency) in matching.iter_mut().zip([440.0, 660.0]) {
            let mut sine = 0.0;
            let mut cosine = 0.0;
            for (index, sample) in samples().enumerate() {
                let phase = std::f64::consts::TAU * frequency * index as f64 / f64::from(rate);
                sine += sample * phase.sin();
                cosine += sample * phase.cos();
            }
            let amplitude = 2.0 * sine.hypot(cosine) / length as f64;
            if amplitude > 500.0 && amplitude > rms * 0.4 {
                *count += 1;
            }
        }
    }
    Ok(matching)
}

impl RuntimeInputs {
    #[cfg(test)]
    pub(crate) fn new(
        ui: SocketAddr,
        server: SocketAddr,
        alice: &Path,
        bob: &Path,
        recording: &Path,
    ) -> io::Result<Self> {
        let inputs = Self {
            format: 1,
            ui,
            server,
            alice: alice.to_owned(),
            bob: bob.to_owned(),
            recording: recording.to_owned(),
        };
        inputs.check()?;
        Ok(inputs)
    }

    fn check(&self) -> io::Result<()> {
        if self.format != 1
            || !self.ui.ip().is_loopback()
            || !self.server.ip().is_loopback()
            || self.ui.port() == 0
            || self.server.port() == 0
            || self.ui == self.server
        {
            return Err(invalid(
                "browser native endpoints differ from their loopback profile",
            ));
        }
        let alice = original(&self.alice, "alice-microphone.wav")?;
        if original(&self.bob, "bob-microphone.wav")? != alice
            || original(&self.recording, "browser-known.wav")? != alice
        {
            return Err(invalid(
                "browser native audio inputs have different data roots",
            ));
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn save(&self, directory: &Path) -> io::Result<()> {
        self.check()?;
        fs::create_dir(directory)?;
        for (path, name) in [
            (&self.alice, "alice-microphone.wav"),
            (&self.bob, "bob-microphone.wav"),
            (&self.recording, "browser-known.wav"),
        ] {
            write(&directory.join(name), &read(path, MAX_AUDIO)?)?;
        }
        write(
            &directory.join(FILES[0]),
            &serde_json::to_vec(self).map_err(io::Error::other)?,
        )?;
        Self::read(directory).map(|_| ())
    }

    pub(crate) fn read(directory: &Path) -> io::Result<Self> {
        if !fs::symlink_metadata(directory)?.is_dir() {
            return Err(invalid(
                "browser runtime artifacts are not a physical directory",
            ));
        }
        let mut expected: BTreeSet<_> = FILES.into_iter().map(str::to_owned).collect();
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let name = entry.file_name();
            if !entry.file_type()?.is_file()
                || !name.to_str().is_some_and(|name| expected.remove(name))
            {
                return Err(invalid("browser runtime artifact inventory differs"));
            }
        }
        if !expected.is_empty() {
            return Err(invalid("browser runtime artifact inventory is incomplete"));
        }
        let inputs: Self = serde_json::from_slice(&read(&directory.join(FILES[0]), 16_384)?)
            .map_err(io::Error::other)?;
        inputs.check()?;
        let alice = matching(&read(&directory.join(FILES[1]), MAX_AUDIO)?, 48000)?;
        let bob = matching(&read(&directory.join(FILES[2]), MAX_AUDIO)?, 48000)?;
        let recording = matching(&read(&directory.join(FILES[3]), MAX_AUDIO)?, 8000)?;
        if alice[0] < 5
            || alice[1] != 0
            || bob[0] != 0
            || bob[1] < 5
            || recording.into_iter().any(|frames| frames < 5)
        {
            return Err(invalid(
                "browser native audio does not preserve both identified senders",
            ));
        }
        Ok(inputs)
    }
}
