use xtask::server_distribution::{ArchiveFormat, ServerCandidateName, ServerTarget};

#[test]
fn server_candidates_bind_five_native_targets_to_the_expected_archive_and_executable(
) -> std::io::Result<()> {
    let expected = [
        (
            "x86_64-unknown-linux-gnu",
            ArchiveFormat::TarGzip,
            "bin/rusterisk",
        ),
        (
            "aarch64-unknown-linux-gnu",
            ArchiveFormat::TarGzip,
            "bin/rusterisk",
        ),
        (
            "aarch64-apple-darwin",
            ArchiveFormat::TarGzip,
            "bin/rusterisk",
        ),
        (
            "x86_64-apple-darwin",
            ArchiveFormat::TarGzip,
            "bin/rusterisk",
        ),
        (
            "x86_64-pc-windows-msvc",
            ArchiveFormat::Zip,
            "bin/rusterisk.exe",
        ),
    ];
    assert_eq!(ServerTarget::ALL.len(), expected.len());
    for ((triple, format, binary), target) in expected.into_iter().zip(ServerTarget::ALL) {
        assert_eq!(
            ServerTarget::parse(triple).map_err(std::io::Error::other)?,
            target
        );
        assert_eq!(target.triple(), triple);
        assert_eq!(target.format(), format);
        assert_eq!(target.binary_path(), binary);
        let name = ServerCandidateName::new("0.1.0", target).map_err(std::io::Error::other)?;
        let root = format!("rusterisk-0.1.0-{triple}");
        assert_eq!(name.root(), root);
        assert_eq!(name.file_name(), format!("{root}.{}", format.extension()));
        assert_eq!(name.target(), target);
        assert_eq!(name.version(), "0.1.0");
        assert_eq!(
            target.required_files(),
            [
                binary,
                "README.md",
                "LICENSE-MIT",
                "LICENSE-APACHE",
                "config/http.conf",
                "config/ari.conf",
                "config/pjsip.conf",
                "config/rtp.conf",
                "config/extensions.conf"
            ]
        );
    }
    Ok(())
}

#[test]
fn candidate_names_reject_unsupported_targets_and_nonportable_versions() {
    for target in [
        "",
        "x86_64-unknown-linux-musl",
        "x86_64-pc-windows-gnu",
        "universal-apple-darwin",
        "../../x86_64-unknown-linux-gnu",
    ] {
        assert!(ServerTarget::parse(target).is_err(), "{target}");
    }
    for version in [
        "",
        "latest",
        "01.0.0",
        "0.1",
        "../0.1.0",
        "0.1.0\n",
        "0.1.0+日本語",
        "0.1.0/target",
    ] {
        assert!(
            ServerCandidateName::new(version, ServerTarget::ALL[0]).is_err(),
            "{version}"
        );
    }
    assert!(
        ServerCandidateName::new(&format!("0.1.0+{}", "a".repeat(128)), ServerTarget::ALL[0])
            .is_err()
    );
    assert!(ServerCandidateName::new("0.1.0-rc.1+build.2", ServerTarget::ALL[0]).is_ok());
}
