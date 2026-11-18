use super::{BrowserCommand, Phase};
use crate::process::ProcessError;
use serde_json::{json, Value};
use std::{
    io,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Fault {
    None,
    MissingNative,
    SkippedNative,
    ChangedSource,
    BlockedArtifacts,
    Timeout,
    Interrupted,
    CleanupOutputCollision,
    #[cfg(unix)]
    BlockedCleanup,
}

pub(super) struct Fixture {
    pub root: PathBuf,
    pub image: String,
    pub fault: Fault,
    pub phases: Vec<Phase>,
    pub cleanups: usize,
    temporary: Option<PathBuf>,
}

fn write(root: &Path, path: &str, bytes: &[u8]) -> io::Result<()> {
    let path = root.join(path);
    std::fs::create_dir_all(
        path.parent()
            .ok_or_else(|| io::Error::other("fixture parent missing"))?,
    )?;
    std::fs::write(path, bytes)
}

fn array<const N: usize>(values: [Value; N]) -> Value {
    Value::Array(values.into_iter().collect())
}

fn report(executed: bool) -> Value {
    let titles = [
        ("calls.spec.ts", "actual outgoing and incoming browser calls exchange known audio and save a recording"),
        ("calls.spec.ts", "cancelling or disconnecting during native capture releases late tracks and preserves reconnect"),
        ("layout.spec.ts", "browser controls preserve monochrome reading, keyboard focus, targets, reflow, enlarged text, and spacing"),
        ("layout.spec.ts", "disabled controls explain unavailable JavaScript and a failed application bundle"),
    ];
    let suites: Vec<_> = titles.iter().enumerate().map(|(index, (file, title))| {
        let results = if executed { array([json!({"status":"passed","retry":0,"errors":[]})]) } else { array([]) };
        let tests = array([json!({"expectedStatus":"passed","status":if executed {"expected"} else {"skipped"},"annotations":[],"results":results})]);
        let specs = array([json!({"id":format!("case-{index}"),"file":file,"title":title,"ok":true,"tests":tests})]);
        json!({"specs":specs})
    }).collect();
    let projects = array([json!({"retries":0,"repeatEach":1,"timeout":60000})]);
    let filtered = array([
        json!({"use":{"browserName":"chromium","channel":"chromium","headless":true,"trace":"off","video":"off","screenshot":"off"}}),
    ]);
    json!({
        "config":{"version":"1.65.1","workers":1,"forbidOnly":true,"fullyParallel":false,"globalTimeout":120000,"projects":projects,"filteredProjects":filtered},
        "suites":suites,"errors":[],
        "stats":{"duration":1000,"expected":if executed {4} else {0},"skipped":if executed {0} else {4},"unexpected":0,"flaky":0}
    })
}

fn project(root: &Path) -> io::Result<()> {
    write(root, "Cargo.toml", b"[workspace]\nmembers=['xtask']\n")?;
    for path in [
        "Cargo.lock",
        "xtask/Cargo.toml",
        "xtask/src/lib.rs",
        "xtask/tests/contract.rs",
        "xtask/fuzz/Cargo.toml",
        "xtask/fuzz/Cargo.lock",
        "xtask/fuzz/src/targets.rs",
    ] {
        write(root, path, b"fixture")?;
    }
    for path in [
        "package.json",
        "package-lock.json",
        "tsconfig.json",
        "eslint.config.mjs",
        "biome.json",
        "playwright.config.ts",
        "index.html",
        "style.css",
        "src/main.ts",
        "tests/call.spec.ts",
        "config/http.conf",
        "config/ari.conf",
        "config/pjsip.conf",
        "config/rtp.conf",
        "config/extensions.conf",
    ] {
        write(root, &format!("examples/browser/{path}"), b"fixture")?;
    }
    for file in ["package.json", "index.mjs", "index.ts"] {
        write(
            root,
            &format!("tests/interop/node-ari-client/project-service-only/{file}"),
            b"fixture",
        )?;
    }
    Ok(())
}

impl Fixture {
    #[cfg(unix)]
    pub fn write_retained_runtime(&self) -> io::Result<()> {
        use crate::browser::{runtime_inputs::RuntimeInputs, runtime_inputs_tests::wave};
        use std::net::{Ipv4Addr, SocketAddr};

        let state = self.working()?.join("state");
        let recordings = state.join("runtime-source/recordings");
        std::fs::create_dir_all(&recordings)?;
        let alice = recordings.join("alice-microphone.wav");
        let bob = recordings.join("bob-microphone.wav");
        let recording = recordings.join("browser-known.wav");
        wave(&alice, 48000, &[440.0])?;
        wave(&bob, 48000, &[660.0])?;
        wave(&recording, 8000, &[440.0, 660.0])?;
        let artifacts = state.join("browser-artifacts");
        std::fs::create_dir(&artifacts)?;
        RuntimeInputs::new(
            SocketAddr::from((Ipv4Addr::LOCALHOST, 48101)),
            SocketAddr::from((Ipv4Addr::LOCALHOST, 48102)),
            &alice,
            &bob,
            &recording,
        )?
        .save(&artifacts.join("runtime"))?;
        let png = [
            137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1,
            8, 6, 0, 0, 0, 31, 21, 196, 137, 0, 0, 0, 11, 73, 68, 65, 84, 120, 156, 99, 96, 0, 2,
            0, 0, 5, 0, 1, 165, 246, 69, 64, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
        ];
        for name in [
            "browser-320.png",
            "browser-768.png",
            "browser-1280.png",
            "browser-landscape.png",
            "browser-enlarged.png",
            "browser-enlarged-spacing.png",
            "browser-forced-colors.png",
        ] {
            std::fs::write(artifacts.join(name), png)?;
        }
        Ok(())
    }

    pub fn working(&self) -> io::Result<&Path> {
        self.temporary
            .as_deref()
            .ok_or_else(|| io::Error::other("fixture working directory missing"))
    }

    pub fn perform(
        &mut self,
        command: &mut Command,
        budget: Duration,
        mode: BrowserCommand,
    ) -> Result<(), ProcessError> {
        let arguments: Vec<_> = command
            .get_args()
            .map(|value| value.to_string_lossy().into_owned())
            .collect();
        assert_eq!(command.get_program(), "docker");
        assert!(!budget.is_zero() && budget.as_secs() <= 900);
        if mode == BrowserCommand::Cleanup {
            assert_eq!(&arguments[..2], ["rm", "--force"]);
            assert_eq!(arguments.len(), 3);
            assert!(arguments[2].starts_with("rusterisk-browser-"));
            self.cleanups += 1;
            return Ok(());
        }
        let name = arguments
            .windows(2)
            .find(|pair| pair[0] == "--name")
            .map(|pair| pair[1].as_str())
            .ok_or(ProcessError::InvalidOutput)?;
        let phase = Phase::ALL
            .into_iter()
            .find(|phase| name.ends_with(&format!("-{}", phase.label())))
            .ok_or(ProcessError::InvalidOutput)?;
        self.phases.push(phase);
        let state = arguments
            .iter()
            .find_map(|argument| {
                argument
                    .strip_prefix("type=bind,source=")
                    .and_then(|value| value.strip_suffix(",target=/state"))
            })
            .map(PathBuf::from)
            .ok_or(ProcessError::InvalidOutput)?;
        self.temporary = state.parent().map(Path::to_owned);
        let text = match phase {
            Phase::RustVersion | Phase::CargoVersion => {
                "release: 1.99.0\nhost: aarch64-unknown-linux-gnu\n"
            }
            Phase::NodeVersion => "v26.13.1\n",
            Phase::NpmVersion => "11.20.0\n",
            Phase::BrowserVersion => "Version 1.65.1\n",
            Phase::Fetch
            | Phase::Install
            | Phase::Types
            | Phase::Check
            | Phase::Unit
            | Phase::Bundle
            | Phase::Compile
            | Phase::Inventory
            | Phase::Run => "fixture\n",
        };
        write(&state, &format!("{}.txt", phase.label()), text.as_bytes())?;
        if matches!(phase, Phase::Inventory | Phase::Run) {
            let bytes =
                serde_json::to_vec(&report(phase == Phase::Run)).map_err(io::Error::other)?;
            write(&state, "browser-results.json", &bytes)?;
        }
        if phase == Phase::Run {
            match self.fault {
                Fault::None => {}
                Fault::MissingNative => std::fs::remove_file(state.join("browser-results.json"))?,
                Fault::SkippedNative => write(
                    &state,
                    "browser-results.json",
                    &serde_json::to_vec(&report(false)).map_err(io::Error::other)?,
                )?,
                Fault::ChangedSource => write(&state, "browser/src/main.ts", b"changed")?,
                Fault::BlockedArtifacts => write(&self.root, ".cache/browser-runs", b"blocked")?,
                Fault::Timeout => return Err(ProcessError::Timeout),
                Fault::Interrupted => return Err(ProcessError::Interrupted),
                Fault::CleanupOutputCollision => {
                    write(&state, "run-cleanup.txt", b"reserved")?;
                    return Err(ProcessError::Interrupted);
                }
                #[cfg(unix)]
                Fault::BlockedCleanup => {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(
                        state.join("browser"),
                        std::fs::Permissions::from_mode(0o555),
                    )?;
                }
            }
        }
        Ok(())
    }

    fn close(self) -> io::Result<()> {
        if let Some(temporary) = self.temporary {
            if temporary.exists() {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let browser = temporary.join("state/browser");
                    if browser.exists() {
                        std::fs::set_permissions(browser, std::fs::Permissions::from_mode(0o755))?;
                    }
                }
                std::fs::remove_dir_all(temporary)?;
            }
        }
        std::fs::remove_dir_all(self.root)
    }
}

#[track_caller]
pub(super) fn using_fixture(
    operation: impl FnOnce(&mut Fixture) -> io::Result<()>,
) -> io::Result<()> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "rusterisk-browser-outcome-{}-{stamp}-{}",
        std::process::id(),
        std::panic::Location::caller().line()
    ));
    std::fs::create_dir(&root)?;
    let mut fixture = Fixture {
        root,
        image: format!("sha256:{}", "a".repeat(64)),
        fault: Fault::None,
        phases: Vec::new(),
        cleanups: 0,
        temporary: None,
    };
    let result = project(&fixture.root).and_then(|()| operation(&mut fixture));
    match (result, fixture.close()) {
        (result, Ok(())) => result,
        (Ok(()), Err(error)) => Err(error),
        (Err(primary), Err(cleanup)) => Err(io::Error::other(format!(
            "{primary}; fixture cleanup failed: {cleanup}"
        ))),
    }
}
