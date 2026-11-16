use super::{CapturedAssurance, CommandMode};
use crate::{core_assurance::Phase, process::ProcessError};
use std::{
    collections::BTreeSet,
    io,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
pub const IMAGE: &str = "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
pub const PHASES: [Phase; 3] = [Phase::MiriSetup, Phase::MiriInventory, Phase::MiriTests];
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Fault {
    None,
    MissingDiscovery,
    Skipped,
    Truncated,
    ChangedOriginal,
    ChangedWorking,
    ChangedManifest,
    AddedSource,
    Interrupted,
    #[cfg(unix)]
    BlockedCleanup,
    #[cfg(unix)]
    LinkedResult,
}
pub struct Fixture {
    pub root: PathBuf,
    pub fault: Fault,
    pub phases: Vec<Phase>,
    pub temporary: Option<PathBuf>,
    pub cleanups: usize,
    suites: Vec<Vec<String>>,
}

impl Fixture {
    pub fn working(&self) -> io::Result<&Path> {
        self.temporary
            .as_deref()
            .ok_or_else(|| io::Error::other("Miri execution missing"))
    }
    pub fn inventory(&mut self, count: usize, names: &BTreeSet<String>) {
        self.suites = (0..count).map(|_| Vec::new()).collect();
        for (index, name) in names.iter().enumerate() {
            self.suites[index % count].push(name.to_owned());
        }
    }
    pub fn report(&self, executed: bool) -> io::Result<Vec<u8>> {
        let mut text = String::new();
        for suite in &self.suites {
            let record = if executed {
                serde_json::json!({"type":"suite","event":"started","test_count":suite.len()})
            } else {
                serde_json::json!({"type":"suite","event":"discovery"})
            };
            text.push_str(&serde_json::to_string(&record).map_err(io::Error::other)?);
            text.push('\n');
            for name in suite {
                if executed {
                    for event in ["started", "ok"] {
                        text.push_str(
                            &serde_json::to_string(
                                &serde_json::json!({"type":"test","event":event,"name":name}),
                            )
                            .map_err(io::Error::other)?,
                        );
                        text.push('\n');
                    }
                } else {
                    text.push_str(&serde_json::to_string(&serde_json::json!({"type":"test","event":"discovered","name":name,"ignore":false})).map_err(io::Error::other)?);
                    text.push('\n');
                }
            }
            let record = if executed {
                serde_json::json!({"type":"suite","event":"ok","passed":suite.len(),"failed":0,"ignored":0,"measured":0,"filtered_out":0})
            } else {
                serde_json::json!({"type":"suite","event":"completed","tests":suite.len(),"benchmarks":0,"total":suite.len(),"ignored":0})
            };
            text.push_str(&serde_json::to_string(&record).map_err(io::Error::other)?);
            text.push('\n');
        }
        Ok(text.into_bytes())
    }
    pub fn perform(
        &mut self,
        command: &mut Command,
        budget: Duration,
        mode: CommandMode,
    ) -> Result<(), ProcessError> {
        assert_eq!(mode, CommandMode::Work);
        assert_eq!(budget.as_secs(), 900);
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
            .find(|phase| name.ends_with(phase.label()))
            .ok_or(ProcessError::InvalidOutput)?;
        self.phases.push(phase);
        let state = args
            .iter()
            .find_map(|arg| {
                arg.strip_prefix("type=bind,source=")
                    .and_then(|path| path.strip_suffix(",target=/state"))
            })
            .map(PathBuf::from)
            .ok_or(ProcessError::InvalidOutput)?;
        self.temporary = state.parent().map(Path::to_owned);
        let source = args
            .iter()
            .find_map(|arg| {
                arg.strip_prefix("type=bind,source=")
                    .and_then(|path| path.strip_suffix(",target=/project,readonly"))
            })
            .map(PathBuf::from)
            .ok_or(ProcessError::InvalidOutput)?;
        let bytes = if phase == Phase::MiriSetup {
            b"Miri setup fixture\n".to_vec()
        } else {
            self.report(phase == Phase::MiriTests)?
        };
        let path = state.join(format!("{}.txt", phase.label()));
        std::fs::write(&path, bytes)?;
        if self.fault == Fault::Interrupted {
            return Err(ProcessError::Interrupted);
        }
        if phase == Phase::MiriInventory && self.fault == Fault::MissingDiscovery {
            std::fs::write(&path, b"{}\n")?;
        }
        if phase == Phase::MiriTests {
            match self.fault {
                Fault::Skipped => std::fs::write(
                    &path,
                    String::from_utf8(self.report(true)?)
                        .map_err(io::Error::other)?
                        .replace("\"event\":\"ok\"", "\"event\":\"ignored\""),
                )?,
                Fault::Truncated => {
                    let mut bytes = self.report(true)?;
                    bytes.truncate(bytes.len() / 2);
                    std::fs::write(&path, bytes)?;
                }
                Fault::ChangedOriginal => std::fs::write(
                    self.working()?.join("original/crates/core/src/lib.rs"),
                    b"changed",
                )?,
                Fault::ChangedWorking => std::fs::write(source.join("src/lib.rs"), b"changed")?,
                Fault::ChangedManifest => std::fs::write(source.join("Cargo.toml"), b"changed")?,
                Fault::AddedSource => std::fs::write(source.join("src/unexpected.rs"), b" ")?,
                #[cfg(unix)]
                Fault::BlockedCleanup => {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o555))?;
                }
                #[cfg(unix)]
                Fault::LinkedResult => {
                    let outside = self.root.join("native.jsonl");
                    std::fs::write(&outside, self.report(true)?)?;
                    std::fs::remove_file(&path)?;
                    std::os::unix::fs::symlink(outside, &path)?;
                }
                Fault::None | Fault::MissingDiscovery | Fault::Interrupted => {}
            }
        }
        Ok(())
    }
    fn close(self) -> io::Result<()> {
        if let Some(path) = self.temporary {
            if path.exists() {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let state = path.join("state");
                    if state.exists() {
                        std::fs::set_permissions(state, std::fs::Permissions::from_mode(0o755))?;
                    }
                }
                std::fs::remove_dir_all(path)?;
            }
        }
        std::fs::remove_dir_all(self.root)
    }
}
#[track_caller]
pub fn using_fixture(test: impl FnOnce(&mut Fixture) -> io::Result<()>) -> io::Result<()> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "rusterisk-miri-fixture-{}-{stamp}-{}",
        std::process::id(),
        std::panic::Location::caller().line()
    ));
    std::fs::create_dir(&root)?;
    let mut fixture = Fixture {
        root,
        fault: Fault::None,
        phases: Vec::new(),
        temporary: None,
        cleanups: 0,
        suites: Vec::new(),
    };
    let operation = (|| {
        let core = Path::new(env!("CARGO_MANIFEST_DIR")).join("../crates/core");
        let snapshot = CapturedAssurance::read(&core).map_err(io::Error::other)?;
        for (path, bytes) in snapshot.inputs() {
            let path = fixture.root.join("crates/core").join(path);
            std::fs::create_dir_all(
                path.parent()
                    .ok_or_else(|| io::Error::other("fixture parent"))?,
            )?;
            std::fs::write(path, bytes)?;
        }
        test(&mut fixture)
    })();
    match (operation, fixture.close()) {
        (result, Ok(())) => result,
        (Ok(()), Err(error)) => Err(error),
        (Err(operation), Err(cleanup)) => Err(io::Error::other(format!(
            "{operation}; Miri fixture cleanup failed: {cleanup}"
        ))),
    }
}
