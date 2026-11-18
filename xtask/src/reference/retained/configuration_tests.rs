use super::{
    CaseContext, CaseRun, CaseRuntime, CleanupOutputs, ReferenceNativeContext, SippContext,
    SippRun, ToolOutputs,
};
use crate::{
    reference::{
        commands::{combine, read, write},
        copy_configuration, ReferenceCase, ReferenceImage, ReferenceRunner,
    },
    server_distribution::ServerRuntimeConfig,
};
use std::{
    fs, io,
    net::{Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

const WEBSOCKET: &[u8] = b"[agent]\ntype=websocket_client\nconnection_type=per_call_config\nuri=ws://host.docker.internal:54323/agent?token=private-query\nprotocols=media\nusername=peer\npassword=test-password\nconnection_timeout=500\nreconnect_interval=500\nreconnect_attempts=4\n";
const CARGO_PROGRESS: &[u8] = b"    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.10s\n     Running tests/reference.rs (target/debug/deps/reference-fixture)\n";
const STATISTICS_HEADER: &str = "StartTime;LastResetTime;CurrentTime;ElapsedTime(P);ElapsedTime(C);TargetRate;CallRate(P);CallRate(C);IncomingCall(P);IncomingCall(C);OutgoingCall(P);OutgoingCall(C);TotalCallCreated;CurrentCall;SuccessfulCall(P);SuccessfulCall(C);FailedCall(P);FailedCall(C);FailedCannotSendMessage(P);FailedCannotSendMessage(C);FailedMaxUDPRetrans(P);FailedMaxUDPRetrans(C);FailedTcpConnect(P);FailedTcpConnect(C);FailedTcpClosed(P);FailedTcpClosed(C);FailedUnexpectedMessage(P);FailedUnexpectedMessage(C);FailedCallRejected(P);FailedCallRejected(C);FailedCmdNotSent(P);FailedCmdNotSent(C);FailedRegexpDoesntMatch(P);FailedRegexpDoesntMatch(C);FailedRegexpShouldntMatch(P);FailedRegexpShouldntMatch(C);FailedRegexpHdrNotFound(P);FailedRegexpHdrNotFound(C);FailedOutboundCongestion(P);FailedOutboundCongestion(C);FailedTimeoutOnRecv(P);FailedTimeoutOnRecv(C);FailedTimeoutOnSend(P);FailedTimeoutOnSend(C);FailedTestDoesntMatch(P);FailedTestDoesntMatch(C);FailedTestShouldntMatch(P);FailedTestShouldntMatch(C);FailedStrcmpDoesntMatch(P);FailedStrcmpDoesntMatch(C);FailedStrcmpShouldntMatch(P);FailedStrcmpShouldntMatch(C);OutOfCallMsgs(P);OutOfCallMsgs(C);DeadCallMsgs(P);DeadCallMsgs(C);Retransmissions(P);Retransmissions(C);AutoAnswered(P);AutoAnswered(C);Warnings(P);Warnings(C);FatalErrors(P);FatalErrors(C);WatchdogMajor(P);WatchdogMajor(C);WatchdogMinor(P);WatchdogMinor(C);CallLength(P);CallLength(C);CallLengthStDev(P);CallLengthStDev(C);";

fn tools() -> ToolOutputs {
    ToolOutputs {
        asterisk: 1,
        sipp: 2,
        sipp_status: 99,
    }
}

fn cleanup() -> CleanupOutputs {
    CleanupOutputs {
        inspection: 9,
        removal: 10,
        absence: 11,
    }
}

fn context(configuration: PathBuf, directory: PathBuf) -> ReferenceNativeContext {
    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, 54321));
    ReferenceNativeContext {
        cases: CaseContext {
            run: CaseRun {
                owner: "configuration-fixture".to_owned(),
                configuration,
                id: "a".repeat(64),
                address,
                media: Ipv4Addr::new(172, 18, 0, 2),
                callback: Ipv4Addr::new(172, 18, 0, 1),
                address_output: 3,
                media_output: 4,
                callback_output: 5,
                tools: tools(),
                cases: [6, 7, 8],
                runtime: None,
            },
            cleanup: cleanup(),
        },
        sipp: SippContext {
            run: SippRun {
                owner: "sipp-configuration-fixture".to_owned(),
                directory,
                id: "b".repeat(64),
                local: Ipv4Addr::new(172, 18, 0, 3),
                callback: Ipv4Addr::new(172, 18, 0, 1),
                local_output: 3,
                callback_output: 4,
                http: address,
                sip: SocketAddr::from((Ipv4Addr::UNSPECIFIED, 54322)),
                tools: tools(),
                statistics: [5, 8],
                version: 6,
                configuration: 7,
            },
            cleanup: cleanup(),
        },
    }
}

fn prepare_case_runtime(configuration: &Path) -> io::Result<()> {
    let bytes = fs::read(configuration.join("moh/default/known.wav"))?;
    for root in [configuration.to_owned(), configuration.join("native/data")] {
        for name in [
            "en/rusterisk-position.wav",
            "en/digits/rusterisk-one.wav",
            "ja/digits/rusterisk-one.wav",
            "en/rusterisk-welcome.wav",
        ] {
            let path = root.join("sounds").join(name);
            fs::create_dir_all(
                path.parent()
                    .ok_or_else(|| io::Error::other("sound parent missing"))?,
            )?;
            write(&path, &bytes)?;
        }
    }
    write(&configuration.join("native/data/.rusterisk.lock"), b"")?;
    fs::create_dir(configuration.join("native/data/recordings"))
}

fn prepare_sipp(captured: &Path, directory: &Path, context: &SippRun) -> io::Result<()> {
    fs::create_dir(directory)?;
    let configuration = directory.join("asterisk");
    copy_configuration(&captured.join("tests/interop/asterisk"), &configuration)?;
    fs::create_dir(configuration.join("sounds"))?;
    write(
        &configuration.join("sipp-registration.xml"),
        &read(
            &captured.join("tests/interop/sipp/registration.xml"),
            65_536,
        )?,
    )?;
    let native = directory.join("native");
    fs::create_dir(&native)?;
    let expected = ServerRuntimeConfig::for_registration_comparison(
        context.http,
        context.sip.port(),
        |path| read(&captured.join(path), 65_536),
    )?;
    for (path, bytes) in expected.files() {
        let path = native.join(path);
        fs::create_dir_all(
            path.parent()
                .ok_or_else(|| io::Error::other("native configuration parent missing"))?,
        )?;
        write(&path, bytes)?;
    }
    fs::create_dir(native.join("data"))?;
    write(
        &native.join("data/calls.sqlite3"),
        b"mutable native storage fixture",
    )
}

fn report(root: &Path, index: u8, stdout: &[u8], stderr: &[u8]) -> io::Result<()> {
    write(&root.join(format!("{index:03}.stdout")), stdout)?;
    write(&root.join(format!("{index:03}.stderr")), stderr)
}

fn prepare_tools(root: &Path, outputs: &ToolOutputs) -> io::Result<()> {
    report(root, outputs.asterisk, b"Asterisk 22.9.0\n", b"")?;
    report(root, outputs.sipp, b" SIPp v3.8.0-TLS.\n", b"")
}

fn prepare_cleanup(
    root: &Path,
    outputs: &CleanupOutputs,
    owner: &str,
    id: &str,
    image: &ReferenceImage,
) -> io::Result<()> {
    let inspection = serde_json::json!({
        "id": id,
        "requested_image": image.as_str(),
        "name": format!("/rusterisk-interop-{owner}"),
        "owner": owner,
    });
    report(
        root,
        outputs.inspection,
        inspection.to_string().as_bytes(),
        b"",
    )?;
    report(root, outputs.removal, format!("{id}\n").as_bytes(), b"")?;
    report(root, outputs.absence, b"", b"")
}

fn passed(name: &str) -> String {
    format!("running 1 test\ntest {name} ... ok\n\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 5 filtered out; finished in 1.00s\n")
}

fn prepare_case_reports(
    root: &Path,
    context: &CaseContext,
    image: &ReferenceImage,
) -> io::Result<()> {
    let run = &context.run;
    write(&root.join("reference.cid"), run.id.as_bytes())?;
    prepare_tools(root, &run.tools)?;
    for (index, text) in [
        (run.address_output, format!("{}\n", run.address)),
        (run.media_output, format!("{}\n", run.media)),
        (run.callback_output, format!("{} STREAM\n", run.callback)),
    ] {
        report(root, index, text.as_bytes(), b"")?;
    }
    for (case, index) in ReferenceCase::ALL.into_iter().zip(run.cases) {
        report(root, index, passed(case.name()).as_bytes(), CARGO_PROGRESS)?;
    }
    prepare_cleanup(root, &context.cleanup, &run.owner, &run.id, image)
}

fn statistics() -> String {
    let values = STATISTICS_HEADER
        .split_terminator(';')
        .map(|name| match name {
            "StartTime" | "LastResetTime" | "CurrentTime" => {
                "2026-11-17\t04:39:44.640190\t1794890384.640190"
            }
            "OutgoingCall(P)" | "OutgoingCall(C)" | "TotalCallCreated" | "SuccessfulCall(P)"
            | "SuccessfulCall(C)" => "1",
            _ => "0",
        })
        .collect::<Vec<_>>();
    format!("{STATISTICS_HEADER}\n{};\n", values.join(";"))
}

fn prepare_sipp_reports(
    root: &Path,
    context: &SippContext,
    image: &ReferenceImage,
) -> io::Result<()> {
    let run = &context.run;
    write(&root.join("reference.cid"), run.id.as_bytes())?;
    prepare_tools(root, &run.tools)?;
    for (index, text) in [
        (run.local_output, format!("{}\n", run.local)),
        (run.callback_output, format!("{} STREAM\n", run.callback)),
        (
            run.version,
            format!("rusterisk {}\n", env!("CARGO_PKG_VERSION")),
        ),
        (run.configuration, "Configuration valid.\n".to_owned()),
    ] {
        report(root, index, text.as_bytes(), b"")?;
    }
    for index in run.statistics {
        report(root, index, statistics().as_bytes(), b"")?;
    }
    let native = root.join("native");
    let ready = serde_json::json!({
        "fields": {"message": "server ready", "http": run.http},
    });
    write(
        &native.join("native.stdout"),
        format!("{ready}\n").as_bytes(),
    )?;
    write(&native.join("native.stderr"), b"")?;
    prepare_cleanup(root, &context.cleanup, &run.owner, &run.id, image)
}

struct ConfigurationFixture {
    context: ReferenceNativeContext,
    retained: PathBuf,
    captured: PathBuf,
    original_source: PathBuf,
    image: ReferenceImage,
}

impl ConfigurationFixture {
    fn validate(&self) -> io::Result<()> {
        self.context.validate(
            &self.retained,
            &self.original_source,
            &self.image,
            env!("CARGO_PKG_VERSION"),
            &self.captured,
        )
    }
}

fn prepare(root: &Path) -> io::Result<ConfigurationFixture> {
    let captured = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .ok_or_else(|| io::Error::other("workspace parent missing"))?
        .to_owned();
    let configuration = root.join("cases");
    ReferenceRunner::prepare_configuration(
        &captured.join("tests/interop/asterisk"),
        &configuration,
    )?;
    let state = root.join("state");
    fs::create_dir(&state)?;
    let comparisons = state.join("reference-comparisons");
    fs::create_dir(&comparisons)?;
    super::preserve_initial(&configuration, &comparisons)?;
    fs::write(configuration.join("websocket_client.conf"), WEBSOCKET)?;
    for name in ["calls.sqlite3", "calls.sqlite3-wal", "calls.sqlite3-shm"] {
        write(
            &configuration.join("native/data").join(name),
            b"mutable native storage fixture",
        )?;
    }
    prepare_case_runtime(&configuration)?;
    let mut context = context(configuration, state.join("sipp"));
    context.cases.run.runtime = Some(CaseRuntime::capture(
        &captured,
        &context.cases.run.configuration,
        context.cases.run.callback,
    )?);
    prepare_sipp(&captured, &state.join("sipp"), &context.sipp.run)?;
    let image = ReferenceImage::parse_inspection(&format!("sha256:{}\n", "c".repeat(64)))
        .map_err(io::Error::other)?;
    let inspection = state.join("reference-image");
    fs::create_dir(&inspection)?;
    report(
        &inspection,
        1,
        format!("{}\n", image.as_str()).as_bytes(),
        b"",
    )?;
    prepare_case_reports(&comparisons, &context.cases, &image)?;
    prepare_sipp_reports(&state.join("sipp"), &context.sipp, &image)?;
    let retained = root.join("retained");
    context.preserve(&state, &retained)?;
    let original_source = root.join("source");
    context.cases.run.configuration =
        ReferenceRunner::configuration_path(&original_source, &context.cases.run.owner)?;
    Ok(ConfigurationFixture {
        context,
        retained,
        captured,
        original_source,
        image,
    })
}

fn using_fixture(check: impl FnOnce(&ConfigurationFixture) -> io::Result<()>) -> io::Result<()> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    let root = fs::canonicalize(std::env::temp_dir())?.join(format!(
        "rusterisk-reference-retained-config-{}-{stamp}",
        std::process::id()
    ));
    fs::create_dir(&root)?;
    let operation = prepare(&root).and_then(|fixture| check(&fixture));
    combine(
        operation,
        fs::remove_dir_all(root),
        "retained configuration fixture cleanup",
    )
}

fn reject_changes(fixture: &ConfigurationFixture) -> io::Result<()> {
    let retained = fixture.retained.as_path();
    let captured = fixture.captured.as_path();
    fs::write(
        retained.join("case-configuration/unknown.conf"),
        b"unknown configuration",
    )?;
    assert!(fixture
        .context
        .check_configuration(retained, captured)
        .is_err());
    fs::remove_file(retained.join("case-configuration/unknown.conf"))?;
    let altered = std::str::from_utf8(WEBSOCKET)
        .map_err(io::Error::other)?
        .replace(":54323/", ":54324/");
    fs::write(
        retained.join("case-configuration/websocket_client.conf"),
        altered,
    )?;
    assert!(fixture
        .context
        .check_configuration(retained, captured)
        .is_err());
    fs::write(
        retained.join("case-configuration/websocket_client.conf"),
        WEBSOCKET,
    )?;
    fs::write(
        retained.join("case-configuration/ari.conf"),
        b"changed static configuration",
    )?;
    assert!(fixture
        .context
        .check_configuration(retained, captured)
        .is_err());
    Ok(())
}

#[test]
fn retained_case_configuration_keeps_runtime_websocket_settings_and_excludes_generated_databases(
) -> io::Result<()> {
    using_fixture(|fixture| {
        let context = &fixture.context;
        let retained = fixture.retained.as_path();
        let captured = fixture.captured.as_path();
        let websocket = WEBSOCKET;
        context.check_configuration(retained, captured)?;
        for name in ["calls.sqlite3", "calls.sqlite3-wal", "calls.sqlite3-shm"] {
            assert!(!retained
                .join("case-configuration/native/data")
                .join(name)
                .exists());
        }
        assert!(!retained.join("sipp/native/data").exists());
        assert_eq!(
            fs::read(retained.join("case-configuration/websocket_client.conf"))?,
            websocket
        );
        assert_eq!(
            fs::read(retained.join("case-configuration/native/data/moh/default/known.wav"))?,
            fs::read(retained.join("case-configuration/moh/default/known.wav"))?
        );
        reject_changes(fixture)
    })
}

#[test]
fn retained_cases_accept_successful_stdout_with_cargo_progress_on_stderr() -> io::Result<()> {
    using_fixture(|fixture| {
        let root = fixture.retained.join("reference-comparisons");
        for index in fixture.context.cases.run.cases {
            let stderr = root.join(format!("{index:03}.stderr"));
            assert_eq!(read(&stderr, 1024 * 1024)?, CARGO_PROGRESS);
            fs::write(stderr, b"")?;
        }
        fixture.validate()?;
        for (case, index) in ReferenceCase::ALL
            .into_iter()
            .zip(fixture.context.cases.run.cases)
        {
            let stdout = root.join(format!("{index:03}.stdout"));
            let stderr = root.join(format!("{index:03}.stderr"));
            let valid = passed(case.name());
            fs::write(&stderr, CARGO_PROGRESS)?;
            fixture.validate().map_err(|error| {
                io::Error::other(format!(
                    "successful retained case {} rejected Cargo progress: {error}",
                    case.name()
                ))
            })?;
            for invalid in [
                valid.replace("1 passed; 0 failed", "0 passed; 1 failed"),
                passed("a_different_reference_test"),
            ] {
                fs::write(&stdout, invalid)?;
                let error = fixture.validate().err().ok_or_else(|| {
                    io::Error::other("invalid retained case passed with Cargo progress")
                })?;
                assert!(error.to_string().contains("did not pass"));
            }
            fs::write(stdout, valid)?;
            fs::write(stderr, b"")?;
        }
        fixture.validate()
    })
}
