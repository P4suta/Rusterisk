use std::io::{self, Cursor};
use xtask::server_distribution::{
    check_server_oci_archive, OciArchiveLimits, ServerOciContents, ServerTarget,
};

#[path = "oci_archive/fixture.rs"]
mod fixture;
use fixture::{Attestation, Entry, Image};

fn check(image: &Image, bytes: &[u8], limits: OciArchiveLimits) -> io::Result<ServerOciContents> {
    check_server_oci_archive(&image.profile, image.files(), Cursor::new(bytes), limits)
        .map_err(io::Error::other)
}

#[test]
fn complete_candidate_exports_accept_nested_indexes_and_both_attestation_formats() -> io::Result<()>
{
    for target in [ServerTarget::LinuxArm64, ServerTarget::LinuxX86_64] {
        for attestation in [
            Attestation::None,
            Attestation::Legacy,
            Attestation::Artifact,
        ] {
            let mut image = Image::new(target)?;
            image.attestation = attestation;
            for nested in [false, true] {
                image.nested = nested;
                let contents = check(&image, &image.bytes()?, OciArchiveLimits::SERVER)?;
                assert!(contents.manifest_digest().starts_with("sha256:"));
                assert!(contents.config_digest().starts_with("sha256:"));
                assert!(contents.application_layer_digest().starts_with("sha256:"));
                assert_eq!(
                    contents.attestation_count(),
                    usize::from(!matches!(attestation, Attestation::None))
                );
            }
        }
    }
    Ok(())
}

#[test]
fn changed_candidate_file_bytes_modes_and_numeric_owners_are_rejected() -> io::Result<()> {
    for alteration in 0..4 {
        let mut image = Image::new(ServerTarget::LinuxArm64)?;
        let binary = image
            .application
            .iter_mut()
            .find(|entry| entry.path.ends_with("bin/rusterisk"))
            .ok_or_else(|| io::Error::other("fixture binary"))?;
        match alteration {
            0 => binary.bytes.push(b'x'),
            1 => binary.mode = 0o644,
            2 => binary.uid = 0,
            3 => binary.gid = 0,
            _ => return Err(io::Error::other("fixture alteration")),
        }
        assert!(check(&image, &image.bytes()?, OciArchiveLimits::SERVER).is_err());
    }
    Ok(())
}

#[test]
fn changed_execution_settings_labels_source_and_diff_ids_are_rejected() -> io::Result<()> {
    let patches: [fn(&mut serde_json::Value); 9] = [
        |value| value["architecture"] = "amd64".into(),
        |value| value["config"]["User"] = "0:0".into(),
        |value| value["config"]["Entrypoint"][0] = "/bin/sh".into(),
        |value| value["config"]["Cmd"][0] = "--help".into(),
        |value| value["config"]["WorkingDir"] = "/tmp".into(),
        |value| value["config"]["ExposedPorts"] = serde_json::json!({"9999/tcp":{}}),
        |value| value["config"]["Labels"]["org.opencontainers.image.version"] = "9.0.0".into(),
        |value| value["config"]["Labels"]["io.rusterisk.source.sha256"] = "b".repeat(64).into(),
        |value| value["rootfs"]["diff_ids"][0] = format!("sha256:{}", "0".repeat(64)).into(),
    ];
    for patch in patches {
        let mut image = Image::new(ServerTarget::LinuxArm64)?;
        image.config_patch = Some(patch);
        assert!(check(&image, &image.bytes()?, OciArchiveLimits::SERVER).is_err());
    }
    Ok(())
}

#[test]
fn graph_work_is_bounded_even_when_shared_empty_indexes_expand_the_traversal() -> io::Result<()> {
    let image = Image::new(ServerTarget::LinuxArm64)?;
    assert!(check(
        &image,
        &fixture::empty_index_graph(&image)?,
        OciArchiveLimits::SERVER
    )
    .is_err());
    Ok(())
}

#[test]
fn local_exports_reject_embedded_or_external_descriptor_content() -> io::Result<()> {
    let patches: [fn(&mut serde_json::Value); 2] = [
        |value| value["manifests"][0]["data"] = "ZGlmZmVyZW50".into(),
        |value| {
            value["manifests"][0]["urls"] = serde_json::Value::Array(
                [serde_json::Value::String(
                    "https://example.invalid/blob".into(),
                )]
                .into(),
            )
        },
    ];
    for patch in patches {
        let mut image = Image::new(ServerTarget::LinuxArm64)?;
        image.index_patch = Some(patch);
        assert!(check(&image, &image.bytes()?, OciArchiveLimits::SERVER).is_err());
    }
    Ok(())
}

#[test]
fn image_configuration_variants_must_match_the_selected_platform() -> io::Result<()> {
    let mut image = Image::new(ServerTarget::LinuxArm64)?;
    image.config_patch = Some(|value| value["variant"] = "v7".into());
    assert!(check(&image, &image.bytes()?, OciArchiveLimits::SERVER).is_err());
    image.config_patch = Some(|value| value["variant"] = "v8".into());
    check(&image, &image.bytes()?, OciArchiveLimits::SERVER)?;
    Ok(())
}

#[test]
fn exact_archive_blob_entry_and_total_expansion_limits_accept_the_complete_export() -> io::Result<()>
{
    let image = Image::new(ServerTarget::LinuxArm64)?;
    let entries = image.entries()?;
    let bytes = fixture::tar(&entries)?;
    let blob = entries
        .iter()
        .map(|entry| entry.bytes.len() as u64)
        .max()
        .ok_or_else(|| io::Error::other("fixture blob"))?;
    let json = entries
        .iter()
        .filter(|entry| entry.bytes.first() == Some(&b'{'))
        .map(|entry| entry.bytes.len() as u64)
        .max()
        .ok_or_else(|| io::Error::other("fixture JSON"))?;
    let limits = OciArchiveLimits::SERVER.constrain(
        bytes.len() as u64,
        blob,
        json,
        entries.len(),
        image.expanded_bytes()?,
    )?;
    check(&image, &bytes, limits)?;
    let less = limits.constrain(
        bytes.len() as u64,
        blob,
        json,
        entries.len(),
        image.expanded_bytes()? - 1,
    )?;
    assert!(check(&image, &bytes, less).is_err());
    Ok(())
}

#[test]
fn expected_inventory_stops_after_the_tenth_entry_and_layer_gzip_has_no_trailing_member(
) -> io::Result<()> {
    let mut image = Image::new(ServerTarget::LinuxArm64)?;
    let bytes = image.bytes()?;
    let mut extra = 0;
    let unexpected = std::iter::repeat_with(|| {
        extra += 1;
        assert!(extra <= 1);
        ("extra", b"x".as_slice())
    });
    assert!(check_server_oci_archive(
        &image.profile,
        image.files().chain(unexpected),
        Cursor::new(bytes),
        OciArchiveLimits::SERVER
    )
    .is_err());
    assert_eq!(extra, 1);
    for suffix in [b"trailing".to_vec(), fixture::gzip(b"second member")?] {
        image.application_suffix = suffix;
        assert!(check(&image, &image.bytes()?, OciArchiveLimits::SERVER).is_err());
    }
    Ok(())
}

#[test]
fn missing_duplicate_unsafe_and_unexpected_outer_entries_are_rejected() -> io::Result<()> {
    let image = Image::new(ServerTarget::LinuxArm64)?;
    let valid = image.entries()?;
    let mut missing = valid.clone();
    missing.retain(|entry| entry.path != "oci-layout");
    assert!(check(&image, &fixture::tar(&missing)?, OciArchiveLimits::SERVER).is_err());
    for path in [
        "index.json",
        "../index.json",
        "/index.json",
        "blobs/sha256/../index.json",
        "unreferenced",
    ] {
        let mut entries = valid.clone();
        entries.push(Entry::file(path, b"extra"));
        assert!(check(&image, &fixture::tar(&entries)?, OciArchiveLimits::SERVER).is_err());
    }
    for kind in [
        tar::EntryType::Symlink,
        tar::EntryType::Link,
        tar::EntryType::Fifo,
        tar::EntryType::Char,
    ] {
        let mut entries = valid.clone();
        let mut entry = Entry::file("unexpected", b"");
        entry.kind = kind;
        entries.push(entry);
        assert!(check(&image, &fixture::tar(&entries)?, OciArchiveLimits::SERVER).is_err());
    }
    Ok(())
}

#[test]
fn missing_duplicate_links_and_populated_data_in_the_application_layer_are_rejected(
) -> io::Result<()> {
    for mode in 0..6 {
        let mut image = Image::new(ServerTarget::LinuxArm64)?;
        match mode {
            0 => image
                .application
                .retain(|entry| !entry.path.ends_with("README.md")),
            1 => image.application.push(
                image
                    .application
                    .last()
                    .ok_or_else(|| io::Error::other("fixture application"))?
                    .clone(),
            ),
            2 => image
                .application
                .retain(|entry| entry.path != "opt/rusterisk/data/"),
            3 => image
                .application
                .push(Entry::file("opt/rusterisk/data/unexpected", b"extra")),
            4 => image
                .application
                .push(Entry::file("opt/rusterisk/../escape", b"extra")),
            5 => {
                let mut link = Entry::file("opt/rusterisk/link", b"");
                link.kind = tar::EntryType::Symlink;
                image.application.push(link);
            }
            _ => return Err(io::Error::other("fixture mode")),
        }
        assert!(check(&image, &image.bytes()?, OciArchiveLimits::SERVER).is_err());
    }
    Ok(())
}

#[test]
fn changed_blob_bytes_descriptor_sizes_references_and_attestation_subjects_are_rejected(
) -> io::Result<()> {
    let image = Image::new(ServerTarget::LinuxArm64)?;
    let mut entries = image.entries()?;
    let blob = entries
        .iter_mut()
        .find(|entry| entry.path.starts_with("blobs/sha256/") && entry.kind.is_file())
        .ok_or_else(|| io::Error::other("fixture blob"))?;
    blob.bytes.push(b'x');
    assert!(check(&image, &fixture::tar(&entries)?, OciArchiveLimits::SERVER).is_err());
    let patches: [fn(&mut serde_json::Value); 2] = [
        |value| value["config"]["size"] = 1.into(),
        |value| value["layers"][0]["digest"] = format!("sha256:{}", "0".repeat(64)).into(),
    ];
    for patch in patches {
        let mut image = Image::new(ServerTarget::LinuxArm64)?;
        image.manifest_patch = Some(patch);
        assert!(check(&image, &image.bytes()?, OciArchiveLimits::SERVER).is_err());
    }
    let mut image = Image::new(ServerTarget::LinuxArm64)?;
    image.attestation_patch =
        Some(|value| value["subject"]["digest"] = format!("sha256:{}", "0".repeat(64)).into());
    assert!(check(&image, &image.bytes()?, OciArchiveLimits::SERVER).is_err());
    Ok(())
}

#[test]
fn duplicate_label_keys_are_rejected_and_invalid_metadata_diagnostics_are_sanitized(
) -> io::Result<()> {
    let mut image = Image::new(ServerTarget::LinuxArm64)?;
    image.config_bytes_patch = Some(|bytes| {
        let text = String::from_utf8(bytes).map_err(io::Error::other)?;
        let needle = r#""org.opencontainers.image.version":"0.1.0""#;
        if text.matches(needle).count() != 1 {
            return Err(io::Error::other("fixture version label"));
        }
        Ok(text
            .replacen(needle, &format!("{needle},{needle}"), 1)
            .into_bytes())
    });
    assert!(check(&image, &image.bytes()?, OciArchiveLimits::SERVER).is_err());
    image.config_bytes_patch = None;
    image.config_patch = Some(|value| value["config"]["Cmd"] = "PRIVATE_FIXTURE_VALUE".into());
    match check(&image, &image.bytes()?, OciArchiveLimits::SERVER) {
        Ok(_) => Err(io::Error::other("invalid fixture metadata accepted")),
        Err(error) => {
            assert!(!error.to_string().contains("PRIVATE_FIXTURE_VALUE"));
            assert!(!format!("{error:?}").contains("PRIVATE_FIXTURE_VALUE"));
            Ok(())
        }
    }
}

#[test]
fn unreferenced_valid_blobs_missing_blob_bytes_and_incomplete_tar_termination_are_rejected(
) -> io::Result<()> {
    let image = Image::new(ServerTarget::LinuxArm64)?;
    let mut entries = image.entries()?;
    let bytes = b"unreferenced valid blob";
    let digest = fixture::digest(bytes);
    let hex = digest
        .strip_prefix("sha256:")
        .ok_or_else(|| io::Error::other("fixture digest"))?;
    entries.push(Entry::file(&format!("blobs/sha256/{hex}"), bytes));
    assert!(check(&image, &fixture::tar(&entries)?, OciArchiveLimits::SERVER).is_err());
    let mut entries = image.entries()?;
    let removed = entries
        .iter()
        .position(|entry| entry.path.starts_with("blobs/sha256/") && entry.kind.is_file())
        .ok_or_else(|| io::Error::other("fixture blob"))?;
    entries.remove(removed);
    assert!(check(&image, &fixture::tar(&entries)?, OciArchiveLimits::SERVER).is_err());
    let mut bytes = image.bytes()?;
    bytes.truncate(bytes.len() - 512);
    assert!(check(&image, &bytes, OciArchiveLimits::SERVER).is_err());
    Ok(())
}

#[test]
fn trailing_archives_and_all_configurable_limits_are_rejected() -> io::Result<()> {
    let image = Image::new(ServerTarget::LinuxArm64)?;
    let bytes = image.bytes()?;
    let mut trailing = bytes.clone();
    trailing.push(1);
    assert!(check(&image, &trailing, OciArchiveLimits::SERVER).is_err());
    let entries = image.entries()?;
    let maximum_blob = entries
        .iter()
        .map(|entry| entry.bytes.len() as u64)
        .max()
        .ok_or_else(|| io::Error::other("fixture blobs"))?;
    let limits = OciArchiveLimits::SERVER;
    for limited in [
        limits.constrain(bytes.len() as u64 - 1, maximum_blob, 65_536, 128, 1_048_576)?,
        limits.constrain(bytes.len() as u64, maximum_blob - 1, 65_536, 128, 1_048_576)?,
        limits.constrain(bytes.len() as u64, maximum_blob, 1, 128, 1_048_576)?,
        limits.constrain(bytes.len() as u64, maximum_blob, 65_536, 1, 1_048_576)?,
        limits.constrain(bytes.len() as u64, maximum_blob, 65_536, 128, 1)?,
    ] {
        assert!(check(&image, &bytes, limited).is_err());
    }
    assert!(check_server_oci_archive(
        &image.profile,
        image
            .files()
            .chain(std::iter::once(("extra", b"x".as_slice()))),
        Cursor::new(&bytes),
        limits
    )
    .is_err());
    assert!(limits.constrain(0, 1, 1, 1, 1).is_err());
    Ok(())
}
