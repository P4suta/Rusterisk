use rusterisk_config::{Config, MusicSort};
use std::{
    fs,
    io::{self, Read},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};
use xtask::reference::{copy_configuration, prepare_music_fixture, ReferenceMusicFixture};

const HEADER: [u8; 44] = [
    b'R', b'I', b'F', b'F', 0xa4, 0x3e, 0, 0, b'W', b'A', b'V', b'E', b'f', b'm', b't', b' ', 16,
    0, 0, 0, 1, 0, 1, 0, 0x40, 0x1f, 0, 0, 0x80, 0x3e, 0, 0, 2, 0, 16, 0, b'd', b'a', b't', b'a',
    0x80, 0x3e, 0, 0,
];
static FIXTURE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn verify(condition: bool, reason: &'static str) -> io::Result<()> {
    condition
        .then_some(())
        .ok_or_else(|| io::Error::other(reason))
}

fn read(path: &Path) -> io::Result<Vec<u8>> {
    let metadata = fs::symlink_metadata(path)?;
    verify(
        metadata.is_file() && !metadata.file_type().is_symlink() && metadata.len() <= 65_536,
        "Fixture output is not a bounded regular file",
    )?;
    let mut bytes = Vec::new();
    fs::File::open(path)?.take(65_537).read_to_end(&mut bytes)?;
    verify(
        bytes.len() <= 65_536,
        "Fixture output exceeded its file limit",
    )?;
    Ok(bytes)
}

#[derive(PartialEq, Eq)]
enum Entry {
    Directory(PathBuf),
    File(PathBuf, Vec<u8>),
    Link(PathBuf, PathBuf),
}

impl Entry {
    fn path(&self) -> &Path {
        match self {
            Self::Directory(path) | Self::File(path, _) | Self::Link(path, _) => path,
        }
    }
}

fn tree(root: &Path) -> io::Result<Vec<Entry>> {
    fn walk(
        root: &Path,
        relative: &Path,
        entries: &mut Vec<Entry>,
        bytes: &mut usize,
    ) -> io::Result<()> {
        verify(
            relative.components().count() <= 4,
            "Fixture output exceeded its depth limit",
        )?;
        for entry in fs::read_dir(root.join(relative))? {
            let entry = entry?;
            verify(
                entries.len() < 64,
                "Fixture output exceeded its entry limit",
            )?;
            let path = relative.join(entry.file_name());
            let kind = entry.file_type()?;
            if kind.is_symlink() {
                entries.push(Entry::Link(path.clone(), fs::read_link(root.join(path))?));
            } else if kind.is_dir() {
                entries.push(Entry::Directory(path.clone()));
                walk(root, &path, entries, bytes)?;
            } else {
                let content = read(&root.join(&path))?;
                *bytes = bytes
                    .checked_add(content.len())
                    .ok_or_else(|| io::Error::other("Fixture byte overflow"))?;
                verify(
                    *bytes <= 131_072,
                    "Fixture output exceeded its total byte limit",
                )?;
                entries.push(Entry::File(path, content));
            }
        }
        Ok(())
    }
    let mut entries = Vec::new();
    let mut bytes = 0;
    walk(root, Path::new(""), &mut entries, &mut bytes)?;
    entries.sort_by(|left, right| left.path().cmp(right.path()));
    Ok(entries)
}

fn fixture_path(parent: &Path, stamp: u128) -> PathBuf {
    let sequence = FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    parent.join(format!(
        "rusterisk-reference-music-{}-{stamp}-{sequence}",
        std::process::id()
    ))
}

fn using_fixture(test: impl FnOnce(&Path, &Path) -> io::Result<()>) -> io::Result<()> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    let parent = fixture_path(&fs::canonicalize(std::env::temp_dir())?, stamp);
    fs::create_dir(&parent)?;
    let operation = (|| {
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/interop/asterisk");
        let configuration = parent.join("config");
        copy_configuration(&source, &configuration)?;
        fs::create_dir(configuration.join("sounds"))?;
        let mut silence = [0u8; 16_044];
        silence[..44].copy_from_slice(&HEADER);
        for name in [
            "catalog-first.wav",
            "catalog-second.wav",
            "catalog-third.wav",
        ] {
            fs::write(configuration.join("sounds").join(name), silence)?;
        }
        test(&parent, &configuration)
    })();
    match (operation, fs::remove_dir_all(parent)) {
        (operation, Ok(())) => operation,
        (Ok(()), Err(error)) => Err(error),
        (Err(operation), Err(cleanup)) => Err(io::Error::other(format!(
            "{operation}; reference music fixture cleanup failed: {cleanup}"
        ))),
    }
}

#[test]
fn fixture_paths_differ_within_one_clock_tick() -> io::Result<()> {
    let parent = fs::canonicalize(std::env::temp_dir())?;
    verify(
        fixture_path(&parent, 1) != fixture_path(&parent, 1),
        "Reference music fixture paths collided",
    )
}

fn wave(bytes: &[u8], sample: i16) -> io::Result<()> {
    verify(bytes.len() == 16_044, "Known music WAV length changed")?;
    let header = bytes
        .get(..44)
        .ok_or_else(|| io::Error::other("Known music WAV header missing"))?;
    let payload = bytes
        .get(44..)
        .ok_or_else(|| io::Error::other("Known music WAV payload missing"))?;
    verify(
        header == HEADER,
        "Known music WAV is not mono PCM16 at 8000 Hz",
    )?;
    let (samples, remainder) = payload.as_chunks::<2>();
    verify(
        remainder.is_empty()
            && samples.len() == 8000
            && samples.iter().all(|value| *value == sample.to_le_bytes()),
        "Known music WAV changed its fixed PCM signal",
    )
}

fn music_configuration(source: &str) -> io::Result<Config> {
    Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=test-password\n",
        "",
        "",
        "",
    )
    .and_then(|config| config.with_music_on_hold_configuration(source))
    .map_err(io::Error::other)
}

fn directory<'a>(source: &'a str, class: &str) -> io::Result<&'a str> {
    let mut selected = false;
    let mut directory = None;
    for line in source.lines().map(str::trim) {
        if let Some(name) = line
            .strip_prefix('[')
            .and_then(|name| name.strip_suffix(']'))
        {
            selected = name == class;
        } else if let Some((_, value)) = line
            .split_once('=')
            .filter(|(name, _)| selected && name.trim() == "directory")
        {
            verify(directory.is_none(), "Music directory is ambiguous")?;
            directory = Some(value.trim());
        }
    }
    directory.ok_or_else(|| io::Error::other("Music directory missing"))
}

#[test]
fn fixed_music_outputs_preserve_the_sound_catalog_and_project_both_server_roles() -> io::Result<()>
{
    using_fixture(|_parent, configuration| {
        let mut expected = tree(configuration)?;
        let fixture: ReferenceMusicFixture = prepare_music_fixture(configuration)?;
        let files: Vec<_> = fixture.native_files().take(4).collect();
        verify(
            files.len() == 3,
            "Native music projection changed its file inventory",
        )?;
        let native = |name: &str| -> io::Result<&[u8]> {
            let mut selected = files.iter().filter(|(path, _)| *path == name);
            let (_, bytes) = selected
                .next()
                .ok_or_else(|| io::Error::other("Native music file missing"))?;
            verify(selected.next().is_none(), "Native music file is duplicated")?;
            Ok(bytes)
        };
        let asterisk_bytes = read(&configuration.join("musiconhold.conf"))?;
        let asterisk = std::str::from_utf8(&asterisk_bytes).map_err(io::Error::other)?;
        let native_source =
            std::str::from_utf8(native("config/musiconhold.conf")?).map_err(io::Error::other)?;
        let native_config = music_configuration(native_source)?;
        let asterisk_config = music_configuration(&asterisk.replace("/etc/asterisk/moh/", "moh/"))?;
        verify(
            native_config.music_on_hold.classes().len() == 2
                && asterisk_config.music_on_hold.classes().len() == 2,
            "Music projection changed its class inventory",
        )?;
        for (class, path, sample) in [("default", "default", 1200i16), ("Main", "main", 2400)] {
            let local = format!("moh/{path}/known.wav");
            let bytes = read(&configuration.join(&local))?;
            wave(&bytes, sample)?;
            verify(
                native(&format!("data/{local}"))? == bytes,
                "Native and Asterisk music bytes differ",
            )?;
            verify(
                directory(asterisk, class)? == format!("/etc/asterisk/moh/{path}"),
                "Asterisk music path is not configuration-mounted",
            )?;
            for projected in [&native_config, &asterisk_config] {
                let definition = projected
                    .music_on_hold
                    .class(class)
                    .ok_or_else(|| io::Error::other("Projected music class missing"))?;
                verify(
                    definition.name().as_str() == class
                        && definition.directory().as_str() == format!("moh/{path}")
                        && definition.sort() == MusicSort::Alpha,
                    "Projected music class changed its role or order",
                )?;
            }
            expected.push(Entry::Directory(format!("moh/{path}").into()));
            expected.push(Entry::File(local.into(), bytes));
        }
        expected.push(Entry::Directory("moh".into()));
        expected.push(Entry::File("musiconhold.conf".into(), asterisk_bytes));
        expected.sort_by(|left, right| left.path().cmp(right.path()));
        verify(
            tree(configuration)? == expected,
            "Music preparation changed files outside its bounded outputs",
        )
    })
}

#[test]
fn repeated_preparation_cannot_overwrite_or_reuse_an_existing_music_tree() -> io::Result<()> {
    using_fixture(|_parent, configuration| {
        prepare_music_fixture(configuration)?;
        let before = tree(configuration)?;
        verify(
            prepare_music_fixture(configuration).is_err(),
            "Existing music fixture was reused",
        )?;
        verify(
            tree(configuration)? == before,
            "Repeated preparation changed existing files",
        )
    })
}

enum Collision {
    Configuration,
    MusicDirectory,
    MusicFile,
}

#[test]
fn existing_outputs_survive_rejection_without_partial_owned_writes() -> io::Result<()> {
    for collision in [
        Collision::Configuration,
        Collision::MusicDirectory,
        Collision::MusicFile,
    ] {
        using_fixture(|_parent, configuration| {
            match collision {
                Collision::Configuration => fs::write(
                    configuration.join("musiconhold.conf"),
                    b"existing class configuration",
                )?,
                Collision::MusicDirectory => {
                    fs::create_dir(configuration.join("moh"))?;
                    fs::write(
                        configuration.join("moh/keep"),
                        b"existing directory content",
                    )?;
                }
                Collision::MusicFile => fs::write(configuration.join("moh"), b"existing file")?,
            }
            let before = tree(configuration)?;
            verify(
                prepare_music_fixture(configuration).is_err(),
                "Existing output was replaced",
            )?;
            verify(
                tree(configuration)? == before,
                "Failed preparation changed existing files or retained partial output",
            )
        })?;
    }
    Ok(())
}

#[test]
fn preparation_requires_an_existing_real_configuration_directory() -> io::Result<()> {
    using_fixture(|parent, configuration| {
        let before = tree(configuration)?;
        let missing = parent.join("missing");
        verify(
            prepare_music_fixture(&missing).is_err(),
            "Missing configuration root was accepted",
        )?;
        verify(!missing.exists(), "Missing configuration root was created")?;
        let file = parent.join("regular-file");
        fs::write(&file, b"preserved root bytes")?;
        verify(
            prepare_music_fixture(&file).is_err(),
            "A file was accepted as a configuration root",
        )?;
        verify(
            read(&file)? == b"preserved root bytes",
            "Rejected root file was overwritten",
        )?;
        verify(
            tree(configuration)? == before,
            "Rejected root changed the real configuration",
        )
    })
}

#[cfg(unix)]
#[test]
fn symlinked_configuration_roots_and_ancestors_cannot_redirect_fixture_writes() -> io::Result<()> {
    using_fixture(|parent, configuration| {
        let before = tree(configuration)?;
        let alias = parent.join("configuration-alias");
        std::os::unix::fs::symlink(configuration, &alias)?;
        verify(
            prepare_music_fixture(&alias).is_err(),
            "Symlinked configuration root was followed",
        )?;
        let ancestor = parent.join("ancestor-alias");
        std::os::unix::fs::symlink(parent, &ancestor)?;
        verify(
            prepare_music_fixture(&ancestor.join("config")).is_err(),
            "Symlinked configuration ancestor was followed",
        )?;
        verify(
            tree(configuration)? == before,
            "Rejected symlink root changed the target",
        )
    })
}

#[cfg(unix)]
#[test]
fn symlinked_output_paths_preserve_foreign_files_and_configuration_state() -> io::Result<()> {
    for relative in ["moh", "musiconhold.conf"] {
        using_fixture(|parent, configuration| {
            let foreign = parent.join("foreign");
            fs::create_dir(&foreign)?;
            fs::write(foreign.join("preserved"), b"foreign content")?;
            let target = if relative == "moh" {
                foreign.clone()
            } else {
                foreign.join("preserved")
            };
            std::os::unix::fs::symlink(&target, configuration.join(relative))?;
            let before = tree(configuration)?;
            verify(
                prepare_music_fixture(configuration).is_err(),
                "Symlinked fixture output was followed",
            )?;
            verify(
                tree(configuration)? == before,
                "Failed fixture write replaced a symlink or retained partial output",
            )?;
            let count = fs::read_dir(foreign)?
                .take(2)
                .try_fold(0usize, |count, entry| {
                    entry?;
                    Ok::<_, io::Error>(count + 1)
                })?;
            verify(
                read(&parent.join("foreign/preserved"))? == b"foreign content" && count == 1,
                "Failed fixture write changed a foreign target",
            )
        })?;
    }
    Ok(())
}
