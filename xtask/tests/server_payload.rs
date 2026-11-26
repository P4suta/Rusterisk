use std::collections::BTreeMap;
use xtask::server_distribution::{ServerPayload, ServerTarget};

fn executable() -> Vec<u8> {
    let mut bytes = vec![0; 32];
    bytes[..4].copy_from_slice(&0xfeedfacfu32.to_le_bytes());
    bytes[4..8].copy_from_slice(&0x0100000cu32.to_le_bytes());
    bytes[12..16].copy_from_slice(&2u32.to_le_bytes());
    bytes
}

fn inputs() -> BTreeMap<&'static str, Vec<u8>> {
    let mut files: BTreeMap<_, _> = [
        "docs/server-distribution.md",
        "LICENSE-MIT",
        "LICENSE-APACHE",
        "config/http.conf",
        "config/ari.conf",
        "config/pjsip.conf",
        "config/rtp.conf",
        "config/extensions.conf",
    ]
    .into_iter()
    .map(|path| (path, format!("source bytes for {path}\n").into_bytes()))
    .collect();
    files.insert(
        "Cargo.toml",
        b"[workspace.package]\nversion='0.1.0'\n".to_vec(),
    );
    files
}

fn read(files: &BTreeMap<&str, Vec<u8>>, path: &str) -> std::io::Result<Vec<u8>> {
    files
        .get(path)
        .cloned()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, path.to_owned()))
}

#[test]
fn server_payload_captures_exact_license_configuration_documentation_and_binary_bytes(
) -> std::io::Result<()> {
    let mut source = inputs();
    let binary = executable();
    let mut cli = executable();
    cli.extend_from_slice(b"CLI");
    let capture = ServerPayload::capture(
        ServerTarget::MacOsArm64,
        binary.clone(),
        cli.clone(),
        |path| read(&source, path),
    )
    .map_err(std::io::Error::other)?;
    source.insert("LICENSE-MIT", b"changed after capture".to_vec());
    assert_eq!(capture.name().version(), "0.1.0");
    assert_eq!(
        capture.name().file_name(),
        "rusterisk-0.1.0-aarch64-apple-darwin.tar.gz"
    );
    let captured: BTreeMap<_, _> = capture.files().collect();
    assert_eq!(
        captured.get("bin/rusterisk-cli").copied(),
        Some(cli.as_slice())
    );
    assert_eq!(captured.len(), 10);
    assert_eq!(
        captured.get("bin/rusterisk").copied(),
        Some(binary.as_slice())
    );
    assert_eq!(
        captured.get("LICENSE-MIT").copied(),
        Some(b"source bytes for LICENSE-MIT\n".as_slice())
    );
    assert_eq!(
        captured.get("README.md").copied(),
        source.get("docs/server-distribution.md").map(Vec::as_slice)
    );
    for path in [
        "config/http.conf",
        "config/ari.conf",
        "config/pjsip.conf",
        "config/rtp.conf",
        "config/extensions.conf",
        "LICENSE-APACHE",
    ] {
        assert_eq!(
            captured.get(path).copied(),
            source.get(path).map(Vec::as_slice),
            "{path}"
        );
    }
    Ok(())
}

#[test]
fn payload_capture_rejects_missing_empty_nontext_and_oversize_inputs_and_wrong_binaries() {
    let original = inputs();
    for path in original.keys() {
        for replacement in [
            None,
            Some(Vec::new()),
            Some(vec![b'x'; 65_537]),
            Some(b"bad\0text".to_vec()),
            Some([0xff].to_vec()),
        ] {
            let mut source = original.clone();
            match replacement {
                Some(bytes) => {
                    source.insert(path, bytes);
                }
                None => {
                    source.remove(path);
                }
            }
            assert!(
                ServerPayload::capture(
                    ServerTarget::MacOsArm64,
                    executable(),
                    executable(),
                    |path| read(&source, path)
                )
                .is_err(),
                "{path}"
            );
        }
    }
    for binary in [Vec::new(), b"not an executable".to_vec()] {
        assert!(ServerPayload::capture(
            ServerTarget::MacOsArm64,
            executable(),
            binary.clone(),
            |path| read(&original, path)
        )
        .is_err());
        assert!(
            ServerPayload::capture(ServerTarget::MacOsArm64, binary, executable(), |path| read(
                &original, path
            ))
            .is_err()
        );
    }
    assert!(ServerPayload::capture(
        ServerTarget::LinuxArm64,
        executable(),
        executable(),
        |path| read(&original, path)
    )
    .is_err());
}
