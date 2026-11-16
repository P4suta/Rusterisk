use xtask::native_sanitizers::{native_archives, validate_instrumented_archive, NativeLibrary};

#[derive(serde::Serialize)]
struct BuildScript<'a> {
    reason: &'static str,
    package_id: String,
    out_dir: &'a str,
    linked_libs: [String; 1],
    linked_paths: [String; 1],
}

fn build() -> Result<String, std::io::Error> {
    let mut records = Vec::new();
    for (package, library) in [
        ("opusic-sys@0.7.7", "opus"),
        ("libsqlite3-sys@0.40.0", "sqlite3"),
        ("aws-lc-sys@0.47.0", "aws_lc_0_47_0_crypto"),
    ] {
        let owner = package.split('@').next().unwrap_or("");
        let directory = format!("/state/target/debug/build/{owner}-fixture/out");
        records.push(
            serde_json::to_string(&BuildScript {
                reason: "build-script-executed",
                package_id: format!(
                    "registry+https://github.com/rust-lang/crates.io-index#{package}"
                ),
                out_dir: &directory,
                linked_libs: [format!("static={library}")],
                linked_paths: [format!("native={directory}/lib")],
            })
            .map_err(std::io::Error::other)?,
        );
    }
    records.push(r#"{"reason":"build-finished","success":true}"#.to_owned());
    Ok(records.join("\n"))
}

#[test]
fn native_archives_follow_the_linked_libraries_of_the_actual_build_scripts(
) -> Result<(), std::io::Error> {
    let archives = native_archives(build()?.as_bytes())?;
    assert_eq!(archives.len(), 3);
    for (archive, (library, filename)) in archives.iter().zip([
        (NativeLibrary::Opus, "libopus.a"),
        (NativeLibrary::Sqlite, "libsqlite3.a"),
        (NativeLibrary::AwsLc, "libaws_lc_0_47_0_crypto.a"),
    ]) {
        assert_eq!(archive.library(), library);
        assert_eq!(archive.paths().len(), 1);
        assert!(archive.paths()[0].ends_with(filename));
    }
    Ok(())
}

#[test]
fn missing_or_redirected_native_archives_cannot_receive_a_successful_verdict(
) -> Result<(), std::io::Error> {
    let original = build()?;
    for changed in [
        String::new(),
        original.replace("\"success\":true", "\"success\":false"),
        original.replace("static=opus", "dylib=opus"),
        original.replace("opusic-sys@0.7.7", "another-sys@0.7.7"),
        original.replace(
            "/state/target/debug/build/opusic-sys-fixture/out/lib",
            "/outside",
        ),
        original.replace(
            "/state/target/debug/build/opusic-sys-fixture/out/lib",
            "/state/target/debug/build/opusic-sys-fixture/out/../outside",
        ),
        original.replace("static=sqlite3", "static=../sqlite3"),
        format!("{original}\n{}", original.lines().next().unwrap_or("")),
    ] {
        assert!(native_archives(changed.as_bytes()).is_err(), "{changed}");
    }
    Ok(())
}

#[test]
fn archive_instrumentation_requires_undefined_references_to_both_sanitizers() {
    let symbols = b"member.o:\n U __asan_report_load8\n U __ubsan_handle_type_mismatch_v1_abort\n";
    assert!(validate_instrumented_archive(symbols).is_ok());
    for output in [
        b"".as_slice(),
        b" U __asan_init\n",
        b" U __asan_report_load8\n",
        b" U __ubsan_handle_type_mismatch_v1_abort\n",
        b" T __asan_report_load8\n T __ubsan_handle_type_mismatch_v1_abort\n",
    ] {
        assert!(validate_instrumented_archive(output).is_err());
    }
}
