use std::{
    fs::{self, Metadata, OpenOptions},
    io::{self, Write},
    path::{Component, Path, PathBuf},
};

const HEADER: [u8; 44] = [
    b'R', b'I', b'F', b'F', 0xa4, 0x3e, 0, 0, b'W', b'A', b'V', b'E', b'f', b'm', b't', b' ', 16,
    0, 0, 0, 1, 0, 1, 0, 0x40, 0x1f, 0, 0, 0x80, 0x3e, 0, 0, 2, 0, 16, 0, b'd', b'a', b't', b'a',
    0x80, 0x3e, 0, 0,
];
const DIRECTORIES: [&str; 3] = ["moh", "moh/default", "moh/main"];
const ASTERISK: &[u8] = b"[default]\nmode=files\ndirectory=/etc/asterisk/moh/default\nsort=alpha\n\n[Main]\nmode=files\ndirectory=/etc/asterisk/moh/main\nsort=alpha\n";
const NATIVE: &[u8] = b"[default]\nmode=files\ndirectory=moh/default\nsort=alpha\n\n[Main]\nmode=files\ndirectory=moh/main\nsort=alpha\n";

pub struct ReferenceMusicFixture {
    default: Vec<u8>,
    main: Vec<u8>,
}

impl ReferenceMusicFixture {
    pub fn native_files(&self) -> impl Iterator<Item = (&str, &[u8])> {
        [
            ("config/musiconhold.conf", NATIVE),
            ("data/moh/default/known.wav", self.default.as_slice()),
            ("data/moh/main/known.wav", self.main.as_slice()),
        ]
        .into_iter()
    }

    fn asterisk_files(&self) -> [(&str, &[u8]); 3] {
        [
            ("moh/default/known.wav", self.default.as_slice()),
            ("moh/main/known.wav", self.main.as_slice()),
            ("musiconhold.conf", ASTERISK),
        ]
    }
}

fn wave(sample: i16) -> Vec<u8> {
    let mut bytes = vec![0; 16_044];
    bytes[..HEADER.len()].copy_from_slice(&HEADER);
    for pair in bytes[HEADER.len()..].as_chunks_mut::<2>().0 {
        pair.copy_from_slice(&sample.to_le_bytes());
    }
    bytes
}

fn reparse_point(metadata: &Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

fn directory(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() || reparse_point(&metadata) {
        return Err(io::Error::other(
            "music fixture requires real directories without symlinks",
        ));
    }
    Ok(())
}

fn root(configuration: &Path) -> io::Result<PathBuf> {
    if configuration.as_os_str().len() > 4096 || configuration.components().take(65).count() > 64 {
        return Err(io::Error::other(
            "music fixture configuration path exceeds capacity",
        ));
    }
    let root = if configuration.is_absolute() {
        configuration.to_owned()
    } else {
        std::env::current_dir()?.join(configuration)
    };
    if root.as_os_str().len() > 4096 || root.components().take(65).count() > 64 {
        return Err(io::Error::other(
            "music fixture configuration path exceeds capacity",
        ));
    }
    let mut checked = PathBuf::new();
    for component in root.components() {
        match component {
            Component::Prefix(prefix) => checked.push(prefix.as_os_str()),
            Component::RootDir | Component::Normal(_) => {
                checked.push(component.as_os_str());
                directory(&checked)?;
            }
            Component::CurDir | Component::ParentDir => {
                return Err(io::Error::other(
                    "music fixture configuration path must not traverse directories",
                ));
            }
        }
    }
    directory(&root)?;
    Ok(root)
}

fn vacant(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "music fixture output already exists",
        )),
    }
}

fn combine<T>(primary: io::Result<T>, cleanup: io::Result<()>) -> io::Result<T> {
    match (primary, cleanup) {
        (primary, Ok(())) => primary,
        (Ok(_), Err(error)) => Err(error),
        (Err(primary), Err(cleanup)) => Err(io::Error::other(format!(
            "{primary}; music fixture cleanup failed: {cleanup}"
        ))),
    }
}

pub fn prepare_music_fixture(configuration: &Path) -> io::Result<ReferenceMusicFixture> {
    let root = root(configuration)?;
    vacant(&root.join("moh"))?;
    vacant(&root.join("musiconhold.conf"))?;
    let fixture = ReferenceMusicFixture {
        default: wave(1200),
        main: wave(2400),
    };
    let mut created_directories = [false; 3];
    let mut created_files = [false; 3];
    let operation = (|| {
        for (relative, created) in DIRECTORIES.into_iter().zip(&mut created_directories) {
            fs::create_dir(root.join(relative))?;
            *created = true;
        }
        for ((relative, bytes), created) in
            fixture.asterisk_files().into_iter().zip(&mut created_files)
        {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(root.join(relative))?;
            *created = true;
            file.write_all(bytes)?;
            file.flush()?;
        }
        Ok(())
    })();
    let error = match operation {
        Ok(()) => return Ok(fixture),
        Err(error) => error,
    };
    let mut result = Err(error);
    for ((relative, _), created) in fixture
        .asterisk_files()
        .into_iter()
        .zip(created_files)
        .rev()
    {
        if created {
            result = combine(result, fs::remove_file(root.join(relative)));
        }
    }
    for (relative, created) in DIRECTORIES.into_iter().zip(created_directories).rev() {
        if created {
            result = combine(result, fs::remove_dir(root.join(relative)));
        }
    }
    result
}
