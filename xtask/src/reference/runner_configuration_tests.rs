use super::ReferenceRunner;
use std::{
    fs, io,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

fn using_configuration(test: impl FnOnce(&Path) -> io::Result<()>) -> io::Result<()> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    let directory = fs::canonicalize(std::env::temp_dir())?.join(format!(
        "rusterisk-reference-preparation-{}-{stamp}",
        std::process::id()
    ));
    fs::create_dir(&directory)?;
    match (test(&directory), fs::remove_dir_all(directory)) {
        (operation, Ok(())) => operation,
        (Ok(()), Err(error)) => Err(error),
        (Err(operation), Err(cleanup)) => Err(io::Error::other(format!(
            "{operation}; reference preparation cleanup failed: {cleanup}"
        ))),
    }
}

#[test]
fn configuration_preparation_writes_both_music_roles_before_container_start() -> io::Result<()> {
    using_configuration(|parent| {
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/interop/asterisk");
        let configuration = parent.join("configuration");
        ReferenceRunner::prepare_configuration(&source, &configuration)?;
        assert_eq!(
            fs::read(configuration.join("ari.conf"))?,
            fs::read(source.join("ari.conf"))?
        );
        assert!(configuration.join("sounds").is_dir());
        let native = configuration.join("native");
        let music = fs::read_to_string(native.join("config/musiconhold.conf"))?;
        let parsed = rusterisk_config::Config::parse(
            "[general]\nbindaddr=127.0.0.1\n",
            "[app]\ntype=user\npassword=test-password\n",
            "",
            "",
            "",
        )
        .and_then(|config| config.with_music_on_hold_configuration(&music))
        .map_err(io::Error::other)?;
        assert_eq!(parsed.music_on_hold.classes().len(), 2);
        for (class, sample) in [("default", 1200i16), ("main", 2400)] {
            let bytes = fs::read(configuration.join(format!("moh/{class}/known.wav")))?;
            assert_eq!(
                fs::read(native.join(format!("data/moh/{class}/known.wav")))?,
                bytes
            );
            assert_eq!(bytes.len(), 16_044);
            assert_eq!(&bytes[..4], b"RIFF");
            assert!(bytes[44..]
                .as_chunks::<2>()
                .0
                .iter()
                .all(|pair| *pair == sample.to_le_bytes()));
        }
        assert!(fs::read_to_string(configuration.join("musiconhold.conf"))?
            .contains("directory=/etc/asterisk/moh/default"));
        assert!(!source.join("moh").exists());
        Ok(())
    })
}

#[test]
fn configuration_preparation_keeps_transfer_inputs_in_reloadable_includes() -> io::Result<()> {
    using_configuration(|parent| {
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/interop/asterisk");
        let configuration = parent.join("configuration");
        ReferenceRunner::prepare_configuration(&source, &configuration)?;
        for name in ["pjsip", "extensions"] {
            let file = format!("{name}.conf");
            assert_eq!(
                fs::read_to_string(configuration.join(&file))?,
                format!("#include {name}-scenario-*.conf\n")
            );
            assert_eq!(
                fs::read(configuration.join(format!("{name}-scenario-fixture.conf")))?,
                fs::read(source.join(file))?
            );
        }
        Ok(())
    })
}

#[test]
fn existing_configuration_survives_rejected_preparation() -> io::Result<()> {
    using_configuration(|parent| {
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/interop/asterisk");
        let configuration = parent.join("configuration");
        fs::create_dir(&configuration)?;
        fs::write(configuration.join("preserved"), b"borrowed fixture")?;
        assert!(ReferenceRunner::prepare_configuration(&source, &configuration).is_err());
        assert_eq!(
            fs::read(configuration.join("preserved"))?,
            b"borrowed fixture"
        );
        assert_eq!(fs::read_dir(&configuration)?.count(), 1);
        Ok(())
    })
}

#[test]
fn failed_music_preparation_removes_only_its_owned_configuration() -> io::Result<()> {
    using_configuration(|parent| {
        let source = parent.join("source");
        let configuration = parent.join("configuration");
        fs::create_dir(&source)?;
        fs::write(source.join("ari.conf"), b"application fixture")?;
        fs::write(source.join("musiconhold.conf"), b"existing source music")?;
        assert!(ReferenceRunner::prepare_configuration(&source, &configuration).is_err());
        assert!(!configuration.exists());
        assert_eq!(fs::read(source.join("ari.conf"))?, b"application fixture");
        assert_eq!(
            fs::read(source.join("musiconhold.conf"))?,
            b"existing source music"
        );
        Ok(())
    })
}
