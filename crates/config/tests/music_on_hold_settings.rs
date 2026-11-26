use rusterisk_config::{Config, ConfigError, MusicSort};

const HTTP: &str = "[general]\nbindaddr=127.0.0.1\n";
const ARI: &str = "[app]\ntype=user\npassword=test-password\n";

#[test]
fn loading_refuses_unsupported_music_mode_at_the_source_line() {
    let result = load_from_file(Some("[default]\nmode=custom\ndirectory=moh\n"));
    assert!(
        matches!(&result, Err(error) if error.file == "musiconhold.conf" && error.line == 2),
        "{result:?}"
    );
}

#[test]
fn missing_and_empty_optional_files_keep_the_five_argument_parser_empty() -> Result<(), ConfigError>
{
    let parsed = base()?;
    assert!(parsed.music_on_hold.classes().is_empty());
    for source in [None, Some(""), Some("; no classes\n[general]\n")] {
        let loaded = load_from_file(source)?;
        assert_eq!(loaded, parsed);
    }
    Ok(())
}

#[test]
fn file_classes_preserve_paths_and_use_case_insensitive_identifiers() -> Result<(), ConfigError> {
    let source = "[GENERAL]\n\n[Default]\nMODE=FiLeS\nDIRECTORY=Music/Customer_A-1.2\nSORT=AlPhA\n[Other]\nmode=files\ndirectory=Music/Other\n";
    let parsed = base()?.with_music_on_hold_configuration(source)?;
    assert_eq!(parsed, load_from_file(Some(source))?);
    let classes = parsed.music_on_hold.classes();
    assert_eq!(classes.len(), 2);
    assert_eq!(classes[0].name().as_str(), "Default");
    assert_eq!(classes[0].directory().as_str(), "Music/Customer_A-1.2");
    assert_eq!(classes[0].directory_line(), 5);
    assert_eq!(classes[0].sort(), MusicSort::Alpha);
    assert_eq!(classes[1].sort(), MusicSort::Alpha);
    assert_eq!(parsed.music_on_hold.class("dEfAuLt"), Some(&classes[0]));
    assert!(parsed.music_on_hold.class("missing").is_none());
    Ok(())
}

#[test]
fn replacement_definitions_replace_the_entire_class_catalog() -> Result<(), ConfigError> {
    let configured =
        base()?.with_music_on_hold_configuration("[first]\nmode=files\ndirectory=Music/First\n")?;
    let replacement = configured
        .clone()
        .with_music_on_hold_configuration("[second]\nmode=files\ndirectory=Music/Second\n")?;
    assert!(replacement.music_on_hold.class("first").is_none());
    assert!(replacement.music_on_hold.class("second").is_some());
    assert_eq!(replacement.music_on_hold.classes().len(), 1);
    let result = configured.clone().with_music_on_hold_configuration(
        "[valid]\nmode=files\ndirectory=moh\n[invalid]\nmode=custom\n",
    );
    refuses(result, 5, "mode")?;
    assert_eq!(configured.music_on_hold.classes().len(), 1);
    assert!(configured.music_on_hold.class("first").is_some());
    let cleared = configured.with_music_on_hold_configuration("")?;
    assert!(cleared.music_on_hold.classes().is_empty());
    Ok(())
}

#[test]
fn unsupported_and_missing_settings_identify_the_offending_line() -> Result<(), ConfigError> {
    for (source, line, reason) in [
        ("[default]\nmode=custom\ndirectory=moh\n", 2, "mode"),
        ("[default]\nmode=playlist\ndirectory=moh\n", 2, "mode"),
        ("[default]\nmode=\ndirectory=moh\n", 2, "mode"),
        ("[default]\ndirectory=moh\n", 1, "mode"),
        ("[default]\nmode=files\n", 1, "directory"),
        ("[default]\nmode=files\ndirectory=\n", 3, "directory"),
        (
            "[default]\nmode=files\ndirectory=moh\nsort=random\n",
            4,
            "sort",
        ),
        (
            "[default]\nmode=files\ndirectory=moh\nsort=randstart\n",
            4,
            "sort",
        ),
        ("[default]\nmode=files\ndirectory=moh\nsort=\n", 4, "sort"),
        (
            "[default]\nmode=files\ndirectory=moh\napplication=player\n",
            4,
            "application",
        ),
        (
            "[default]\nmode=files\ndirectory=moh\nunknown=yes\n",
            4,
            "unknown",
        ),
        ("[general]\ncachertclasses=yes\n", 2, "cachertclasses"),
    ] {
        refuses(
            base()?.with_music_on_hold_configuration(source),
            line,
            reason,
        )?;
    }
    Ok(())
}

#[test]
fn case_folded_duplicate_classes_and_options_are_refused() -> Result<(), ConfigError> {
    for (source, line, reason) in [
        (
            "[default]\nmode=files\ndirectory=moh\n[DEFAULT]\nmode=files\ndirectory=other\n",
            4,
            "duplicate",
        ),
        (
            "[default]\nmode=files\ndirectory=moh\nMODE=files\n",
            4,
            "duplicate",
        ),
        (
            "[default]\nMODE=files\ndirectory=moh\nmode=files\n",
            4,
            "duplicate",
        ),
        ("[general]\n[GENERAL]\n", 2, "duplicate"),
    ] {
        refuses(
            base()?.with_music_on_hold_configuration(source),
            line,
            reason,
        )?;
    }
    Ok(())
}

#[test]
fn directories_require_normal_relative_components() -> Result<(), ConfigError> {
    for directory in [
        "/",
        "/moh",
        ".",
        "..",
        "./moh",
        "../moh",
        "moh/.",
        "moh/..",
        "moh//other",
        "moh/",
        "\\moh",
        "moh\\other",
        "C:/moh",
        "moh:other",
        "moh\tother",
        "moh/\0other",
    ] {
        refuses(
            base()?.with_music_on_hold_configuration(&format!(
                "[default]\nmode=files\ndirectory={directory}\n"
            )),
            3,
            "directory",
        )?;
    }
    for directory in ["Music/Customer Hold", ".music/音楽"] {
        let config = base()?.with_music_on_hold_configuration(&format!(
            "[default]\nmode=files\ndirectory={directory}\n"
        ))?;
        assert_eq!(
            config.music_on_hold.classes()[0].directory().as_str(),
            directory
        );
    }
    Ok(())
}

#[test]
fn directory_components_refuse_windows_reserved_characters() -> Result<(), ConfigError> {
    for character in ['<', '>', ':', '"', '\\', '|', '?', '*'] {
        refuses(
            base()?.with_music_on_hold_configuration(&format!(
                "[default]\nmode=files\ndirectory=moh/a{character}b\n"
            )),
            3,
            "directory",
        )?;
    }
    Ok(())
}

#[test]
fn directory_components_refuse_trailing_ascii_spaces_and_periods() -> Result<(), ConfigError> {
    for directory in ["moh/child.", "moh/child./next", "moh/child /next"] {
        refuses(
            base()?.with_music_on_hold_configuration(&format!(
                "[default]\nmode=files\ndirectory={directory}\n"
            )),
            3,
            "directory",
        )?;
    }
    Ok(())
}

#[test]
fn directory_components_refuse_reserved_device_names_with_extensions() -> Result<(), ConfigError> {
    for device in ["CON", "pRn", "AuX", "nul"] {
        for suffix in ["", ".txt", ".tar.gz"] {
            for directory in [
                format!("moh/{device}{suffix}"),
                format!("{device}{suffix}/moh"),
            ] {
                refuses(
                    base()?.with_music_on_hold_configuration(&format!(
                        "[default]\nmode=files\ndirectory={directory}\n"
                    )),
                    3,
                    "directory",
                )?;
            }
        }
    }
    for prefix in ["COM", "lpt"] {
        for digit in ["1", "2", "3", "4", "5", "6", "7", "8", "9", "¹", "²", "³"] {
            for suffix in ["", ".files"] {
                refuses(
                    base()?.with_music_on_hold_configuration(&format!(
                        "[default]\nmode=files\ndirectory=moh/{prefix}{digit}{suffix}/child\n"
                    )),
                    3,
                    "directory",
                )?;
            }
        }
    }
    Ok(())
}

#[test]
fn portable_components_preserve_unicode_internal_spaces_and_nondevice_names(
) -> Result<(), ConfigError> {
    for component in [
        ".music",
        "音楽",
        "Customer Hold",
        "Customer\u{a0}Hold",
        "COM0",
        "COM10",
        "LPT0",
        "LPT10",
        "COM⁴",
        "CONifer",
        "AUXiliary",
        "NUL-name",
        "COM¹-name",
    ] {
        let directory = format!("Music/{component}/MixedCase");
        let config = base()?.with_music_on_hold_configuration(&format!(
            "[default]\nmode=files\ndirectory={directory}\n"
        ))?;
        assert_eq!(
            config.music_on_hold.classes()[0].directory().as_str(),
            directory
        );
    }
    Ok(())
}

#[test]
fn class_and_directory_byte_limits_include_the_exact_boundary() -> Result<(), ConfigError> {
    let name = "n".repeat(79);
    let config = base()?
        .with_music_on_hold_configuration(&format!("[{name}]\nmode=files\ndirectory=moh\n"))?;
    assert_eq!(config.music_on_hold.classes()[0].name().as_str(), name);
    refuses(
        base()?.with_music_on_hold_configuration(&format!(
            "[{}]\nmode=files\ndirectory=moh\n",
            "n".repeat(80)
        )),
        1,
        "class name",
    )?;
    for directory in ["d".repeat(255), "音".repeat(85)] {
        let config = base()?.with_music_on_hold_configuration(&format!(
            "[default]\nmode=files\ndirectory={directory}\n"
        ))?;
        assert_eq!(
            config.music_on_hold.classes()[0].directory().as_str(),
            directory
        );
    }
    for directory in ["d".repeat(256), "音".repeat(86)] {
        refuses(
            base()?.with_music_on_hold_configuration(&format!(
                "[default]\nmode=files\ndirectory={directory}\n"
            )),
            3,
            "directory",
        )?;
    }
    Ok(())
}

#[test]
fn class_capacity_counts_classes_without_counting_the_general_section() -> Result<(), ConfigError> {
    let mut source = String::from("[general]\n");
    for index in 0..32 {
        source.push_str(&format!("[class-{index}]\nmode=files\ndirectory=moh\n"));
    }
    let config = base()?.with_music_on_hold_configuration(&source)?;
    assert_eq!(config.music_on_hold.classes().len(), 32);
    source.push_str("[overflow]\nmode=files\ndirectory=moh\n");
    refuses(base()?.with_music_on_hold_configuration(&source), 98, "32")?;
    Ok(())
}

#[test]
fn loading_definitions_does_not_require_audio_assets() -> Result<(), ConfigError> {
    let config = load_from_file(Some("[default]\nmode=files\ndirectory=Uncreated/Music\n"))?;
    assert_eq!(
        config.music_on_hold.classes()[0].directory().as_str(),
        "Uncreated/Music"
    );
    Ok(())
}

fn base() -> Result<Config, ConfigError> {
    Config::parse(HTTP, ARI, "", "", "")
}

fn refuses(
    result: Result<Config, ConfigError>,
    line: usize,
    reason: &str,
) -> Result<(), ConfigError> {
    match result {
        Ok(_) => Err(fixture_error(format!(
            "Expected musiconhold.conf:{line} refusal containing {reason}"
        ))),
        Err(error) => {
            assert_eq!(error.file, "musiconhold.conf");
            assert_eq!(error.line, line, "{error}");
            assert!(error.reason.contains(reason), "{error}");
            Ok(())
        }
    }
}

fn fixture_error(error: impl std::fmt::Display) -> ConfigError {
    ConfigError {
        file: "music fixture".into(),
        line: 0,
        reason: error.to_string(),
    }
}

fn load_from_file(source: Option<&str>) -> Result<Config, ConfigError> {
    let directory = tempfile::tempdir().map_err(fixture_error)?;
    let result = (|| {
        for (file, contents) in [
            ("http.conf", HTTP),
            ("ari.conf", ARI),
            ("pjsip.conf", ""),
            ("rtp.conf", ""),
            ("extensions.conf", ""),
        ] {
            std::fs::write(directory.path().join(file), contents).map_err(fixture_error)?;
        }
        if let Some(source) = source {
            std::fs::write(directory.path().join("musiconhold.conf"), source)
                .map_err(fixture_error)?;
        }
        Config::load(directory.path())
    })();
    let cleanup = directory.close().map_err(fixture_error);
    match (result, cleanup) {
        (Ok(config), Ok(())) => Ok(config),
        (Err(error), Ok(())) | (Ok(_), Err(error)) => Err(error),
        (Err(error), Err(cleanup)) => Err(fixture_error(format!(
            "{error}; fixture cleanup failed: {cleanup}"
        ))),
    }
}
