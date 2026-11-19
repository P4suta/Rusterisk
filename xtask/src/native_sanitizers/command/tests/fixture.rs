use crate::{
    native_sanitizers::{CommandMode, Defect, NativeLibrary, Phase, Scope},
    process::ProcessError,
};
use std::{
    io,
    path::{Path, PathBuf},
    process::{Command, ExitStatus},
    time::Duration,
};

mod project {
    use crate as xtask;
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/sdk_candidate_build/fixture.rs"
    ));
}

pub const IMAGE: &str = "sha256:8e26e6eeb29b66c3849295c5606adaed78a2c6bd9ef17644370256c7fa977d20";
pub const PHASES: [Phase; 19] = [
    Phase::ClangVersion,
    Phase::RustVersion,
    Phase::CompileProbe(Defect::Address),
    Phase::RejectProbe(Defect::Address),
    Phase::CompileProbe(Defect::Undefined),
    Phase::RejectProbe(Defect::Undefined),
    Phase::Fetch,
    Phase::Build(Scope::Media),
    Phase::Build(Scope::Storage),
    Phase::Build(Scope::Tls),
    Phase::InspectArchive(NativeLibrary::Opus),
    Phase::InspectArchive(NativeLibrary::Sqlite),
    Phase::InspectArchive(NativeLibrary::AwsLc),
    Phase::Inventory(Scope::Media),
    Phase::Run(Scope::Media),
    Phase::Inventory(Scope::Storage),
    Phase::Run(Scope::Storage),
    Phase::Inventory(Scope::Tls),
    Phase::Run(Scope::Tls),
];

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Fault {
    None,
    WrongProbe,
    AcceptedProbe,
    MissingArchive,
    MissingSymbols,
    MissingInventory(Scope),
    SkippedExecution,
    ChangedWorking,
    ChangedPreserved,
    Interrupted,
    #[cfg(unix)]
    BlockedCleanup,
}

pub struct Fixture<'a> {
    pub root: &'a Path,
    pub destination: PathBuf,
    pub fault: Fault,
    pub phases: Vec<Phase>,
    working: Option<PathBuf>,
}

fn names(scope: Scope) -> &'static [&'static str] {
    match scope {
        Scope::Media=>&["opus_transmits_a_non_silent_known_signal","opus_packets_cover_each_frame_duration_and_the_full_120_millisecond_packet","opus_packet_durations_preserve_decoded_samples_in_complete_mixer_frames"],
        Scope::Storage=>&["history_preserves_channel_identity_across_sessions_and_commits_each_batch_atomically","restart_interrupts_active_calls_and_repairs_recording_headers","failed_replacement_publication_keeps_recoverable_audio_and_other_policies_cannot_replace_it"],
        Scope::Tls=>&["signaling::stream_tests::native_tcp_and_tls_reuse_registered_flows_for_calls_and_release_disconnected_calls","signaling::stream_tests::initial_outbound_connections_use_tcp_or_verified_tls_and_fail_closed"],
    }
}
fn inventory(scope: Scope) -> String {
    let names = names(scope);
    let mut result = String::new();
    for name in names {
        result.push_str(&format!("{name}: test\n"));
        if scope != Scope::Tls {
            result.push_str("1 test, 0 benchmarks\n");
        }
    }
    if scope == Scope::Tls {
        result.push_str(&format!("{} tests, 0 benchmarks\n", names.len()));
    }
    result
}
pub fn execution(scope: Scope) -> String {
    let names = names(scope);
    let mut result = String::new();
    for name in names {
        result.push_str(&format!("test {name} ... ok\n"));
        if scope != Scope::Tls {
            result.push_str("test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s\n");
        }
    }
    if scope == Scope::Tls {
        result.push_str(&format!("test result: ok. {} passed; 0 failed; 0 ignored; 0 measured; 2 filtered out; finished in 0.01s\n",names.len()));
    }
    result
}
fn status(code: u8) -> ProcessError {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        ProcessError::Status(ExitStatus::from_raw(i32::from(code) << 8))
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::ExitStatusExt;
        ProcessError::Status(ExitStatus::from_raw(u32::from(code)))
    }
}
#[derive(serde::Serialize)]
struct BuildScript {
    reason: &'static str,
    package_id: String,
    out_dir: String,
    linked_libs: [String; 1],
    linked_paths: [String; 1],
}

impl Fixture<'_> {
    pub fn working(&self) -> io::Result<&Path> {
        self.working
            .as_deref()
            .ok_or_else(|| io::Error::other("native fixture execution missing"))
    }
    pub fn perform(
        &mut self,
        command: &mut Command,
        budget: Duration,
        mode: CommandMode,
    ) -> Result<(), ProcessError> {
        assert_eq!(mode, CommandMode::Work);
        assert!(budget.as_secs() > 0 && budget.as_secs() <= 900);
        let args: Vec<_> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        let name = args
            .windows(2)
            .find(|pair| pair[0] == "--name")
            .map(|pair| pair[1].as_str())
            .ok_or(ProcessError::InvalidOutput)?;
        let phase = PHASES
            .into_iter()
            .find(|phase| name.ends_with(&format!("-{}", phase.label())))
            .ok_or(ProcessError::InvalidOutput)?;
        let source = command
            .get_current_dir()
            .ok_or(ProcessError::InvalidOutput)?;
        self.working = source.parent().map(Path::to_owned);
        let state = args
            .iter()
            .find_map(|arg| {
                arg.strip_prefix("type=bind,source=")
                    .and_then(|arg| arg.strip_suffix(",target=/state"))
            })
            .map(PathBuf::from)
            .ok_or(ProcessError::InvalidOutput)?;
        self.phases.push(phase);
        let text = match phase {
            Phase::ClangVersion => "Debian clang version 14.0.6\n".to_owned(),
            Phase::RustVersion => format!(
                "release: 1.100.0\nhost: {}-unknown-linux-gnu\n",
                std::env::consts::ARCH
            ),
            Phase::RejectProbe(Defect::Address) => {
                "ERROR: AddressSanitizer: heap-use-after-free\n".to_owned()
            }
            Phase::RejectProbe(Defect::Undefined) => {
                "runtime error: signed integer overflow\n".to_owned()
            }
            Phase::Build(Scope::Tls) => {
                let mut text = String::new();
                for (package, library) in [
                    ("opusic-sys", "opus"),
                    ("libsqlite3-sys", "sqlite3"),
                    ("aws-lc-sys", "aws_lc_0_47_0_crypto"),
                ] {
                    if self.fault == Fault::MissingArchive && package == "aws-lc-sys" {
                        continue;
                    }
                    let relative = format!("target/debug/build/{package}-fixture/out/lib");
                    project::write(&state, &format!("{relative}/lib{library}.a"), b"archive")?;
                    text.push_str(&serde_json::to_string(&BuildScript{reason:"build-script-executed",package_id:format!("registry+https://github.com/rust-lang/crates.io-index#{package}@0.1.0"),out_dir:format!("/state/target/debug/build/{package}-fixture/out"),linked_libs:[format!("static={library}")],linked_paths:[format!("native=/state/{relative}")]}).map_err(io::Error::other)?);
                    text.push('\n');
                }
                text.push_str("{\"reason\":\"build-finished\",\"success\":true}\n");
                text
            }
            Phase::InspectArchive(_) => {
                if self.fault == Fault::MissingSymbols {
                    " U __asan_report_load8\n".to_owned()
                } else {
                    " U __asan_report_load8\n U __ubsan_handle_type_mismatch_v1_abort\n".to_owned()
                }
            }
            Phase::Inventory(scope) => {
                if self.fault == Fault::MissingInventory(scope) {
                    String::new()
                } else {
                    inventory(scope)
                }
            }
            Phase::Run(scope) => {
                if self.fault == Fault::SkippedExecution {
                    execution(scope).replace("0 ignored", "1 ignored")
                } else {
                    execution(scope)
                }
            }
            Phase::CompileProbe(_) | Phase::Fetch | Phase::Build(Scope::Media | Scope::Storage) => {
                String::new()
            }
        };
        std::fs::write(
            self.destination.join(format!("{}.txt", phase.label())),
            text,
        )?;
        if self.fault == Fault::Interrupted {
            return Err(ProcessError::Interrupted);
        }
        if phase == Phase::Run(Scope::Tls) {
            match self.fault {
                Fault::ChangedWorking => {
                    project::write(source, "crates/core/src/lib.rs", b"changed")?
                }
                Fault::ChangedPreserved => project::write(
                    &self.destination.join("source"),
                    "crates/core/src/lib.rs",
                    b"changed",
                )?,
                #[cfg(unix)]
                Fault::BlockedCleanup => {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(
                        source.join("crates/core/src"),
                        std::fs::Permissions::from_mode(0o555),
                    )?;
                }
                Fault::None
                | Fault::WrongProbe
                | Fault::AcceptedProbe
                | Fault::MissingArchive
                | Fault::MissingSymbols
                | Fault::MissingInventory(_)
                | Fault::SkippedExecution
                | Fault::Interrupted => {}
            }
        }
        if let Phase::RejectProbe(defect) = phase {
            if self.fault == Fault::AcceptedProbe {
                return Ok(());
            }
            return Err(status(
                if self.fault == Fault::WrongProbe || defect == Defect::Address {
                    86
                } else {
                    87
                },
            ));
        }
        Ok(())
    }
    fn close(&self) -> io::Result<()> {
        if let Some(path) = &self.working {
            if path.exists() {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let source = path.join("source/crates/core/src");
                    if source.exists() {
                        std::fs::set_permissions(source, std::fs::Permissions::from_mode(0o755))?;
                    }
                }
                std::fs::remove_dir_all(path)?;
            }
        }
        Ok(())
    }
}
#[track_caller]
pub fn using_fixture(test: impl FnOnce(&mut Fixture<'_>) -> io::Result<()>) -> io::Result<()> {
    let label = format!("native-outcome-{}", std::panic::Location::caller().line());
    project::using_project(&label, |root, parent| {
        let mut fixture = Fixture {
            root,
            destination: parent.join("native-result"),
            fault: Fault::None,
            phases: Vec::new(),
            working: None,
        };
        let result = test(&mut fixture);
        match (result, fixture.close()) {
            (result, Ok(())) => result.map_err(Into::into),
            (Ok(()), Err(error)) => Err(error.into()),
            (Err(operation), Err(cleanup)) => Err(io::Error::other(format!(
                "{operation}; native fixture cleanup failed: {cleanup}"
            ))
            .into()),
        }
    })
    .map_err(io::Error::other)
}
