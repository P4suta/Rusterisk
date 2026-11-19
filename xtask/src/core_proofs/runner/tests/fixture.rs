use super::{CapturedCoreProofs, CommandMode};
use crate::{core_proofs::HARNESSES, process::ProcessError};
use std::{
    io,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
pub const IMAGE: &str = "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Fault {
    None,
    FullResult,
    Missing,
    Invalid,
    WrongTool,
    MissingHarness,
    Uncovered,
    Oversized,
    ChangedSource,
    Interrupted,
    TimedOut,
    #[cfg(unix)]
    LinkedResult,
    #[cfg(unix)]
    BlockedCleanup,
}
pub struct Fixture {
    pub root: PathBuf,
    pub fault: Fault,
    pub temporary: Option<PathBuf>,
    pub runs: usize,
    pub cleanups: usize,
}

fn native_result() -> io::Result<Vec<u8>> {
    let solvers = [serde_json::json!({"name":"kissat","version":"4.0.1"})];
    let cbmc: Vec<_> = HARNESSES
        .into_iter()
        .map(|name| serde_json::json!({"harness_id":name,"configuration":{"solver":"kissat"}}))
        .collect();
    let results: Vec<_> = HARNESSES
        .into_iter()
        .map(|name| {
            let checks: Vec<_> = [
                serde_json::json!({"status":"Success","category":"assertion"}),
                serde_json::json!({"status":"Satisfied","category":"cover"}),
            ]
            .into_iter()
            .collect();
            serde_json::json!({"harness_id":name,"status":"Success","checks":checks})
        })
        .collect();
    serde_json::to_vec(&serde_json::json!({"metadata":{"version":"1.0","kani_version":"0.68.0"},"tools":{"kani":"0.68.0","cbmc":"6.11.0","solvers":solvers},"cbmc":cbmc,"verification_results":{"summary":{"total_harnesses":HARNESSES.len(),"executed":HARNESSES.len(),"successful":HARNESSES.len(),"failed":0,"status":"completed"},"results":results}})).map_err(io::Error::other)
}
impl Fixture {
    pub fn working(&self) -> io::Result<&Path> {
        self.temporary
            .as_deref()
            .ok_or_else(|| io::Error::other("proof working directory missing"))
    }
    pub(in crate::core_proofs::runner) fn perform(
        &mut self,
        command: &mut Command,
        budget: Duration,
        mode: CommandMode,
    ) -> Result<(), ProcessError> {
        let args: Vec<_> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(command.get_program(), "docker");
        match mode {
            CommandMode::Cleanup => {
                self.cleanups += 1;
                assert_eq!(budget.as_secs(), 30);
                assert_eq!(&args[..3], ["rm", "--force", "--volumes"]);
                assert_eq!(args.len(), 4);
                assert!(args[3].starts_with("rusterisk-core-proofs-"));
                return Ok(());
            }
            CommandMode::Work => {}
        }
        assert_eq!(budget.as_secs(), 900);
        self.runs += 1;
        let results = args
            .iter()
            .find_map(|arg| {
                arg.strip_prefix("type=bind,source=")
                    .and_then(|path| path.strip_suffix(",target=/results"))
            })
            .map(PathBuf::from)
            .ok_or(ProcessError::InvalidOutput)?;
        self.temporary = results.parent().map(Path::to_owned);
        let source = args
            .iter()
            .find_map(|arg| {
                arg.strip_prefix("type=bind,source=")
                    .and_then(|path| path.strip_suffix(",target=/project,readonly"))
            })
            .map(PathBuf::from)
            .ok_or(ProcessError::InvalidOutput)?;
        let bytes = native_result()?;
        let text = String::from_utf8(bytes).map_err(io::Error::other)?;
        let result = results.join("kani.json");
        match self.fault {
            Fault::None | Fault::Interrupted | Fault::TimedOut => std::fs::write(&result, text)?,
            Fault::FullResult => {
                let mut bytes = text.into_bytes();
                bytes.resize(4 * 1024 * 1024, b' ');
                std::fs::write(&result, bytes)?;
            }
            Fault::Missing => {}
            Fault::Invalid => std::fs::write(&result, b"{}")?,
            Fault::WrongTool => std::fs::write(&result, text.replace("6.11.0", "6.10.0"))?,
            Fault::MissingHarness => {
                let mut report: serde_json::Value =
                    serde_json::from_str(&text).map_err(io::Error::other)?;
                let cases = report
                    .pointer_mut("/verification_results/results")
                    .and_then(serde_json::Value::as_array_mut)
                    .ok_or(ProcessError::InvalidOutput)?;
                cases.pop().ok_or(ProcessError::InvalidOutput)?;
                std::fs::write(
                    &result,
                    serde_json::to_vec(&report).map_err(io::Error::other)?,
                )?;
            }
            Fault::Uncovered => std::fs::write(&result, text.replace("Satisfied", "Uncovered"))?,
            Fault::Oversized => std::fs::File::create(&result)?.set_len(4 * 1024 * 1024 + 1)?,
            Fault::ChangedSource => {
                std::fs::write(&result, text)?;
                std::fs::write(source.join("src/lib.rs"), b"changed captured source")?;
            }
            #[cfg(unix)]
            Fault::LinkedResult => {
                let outside = self.root.join("native.json");
                std::fs::write(&outside, text)?;
                std::os::unix::fs::symlink(outside, &result)?;
            }
            #[cfg(unix)]
            Fault::BlockedCleanup => {
                use std::os::unix::fs::PermissionsExt;
                std::fs::write(&result, text)?;
                std::fs::set_permissions(&results, std::fs::Permissions::from_mode(0o555))?;
            }
        }
        match self.fault {
            Fault::Interrupted => Err(ProcessError::Interrupted),
            Fault::TimedOut => Err(ProcessError::Timeout),
            Fault::None
            | Fault::FullResult
            | Fault::Missing
            | Fault::Invalid
            | Fault::WrongTool
            | Fault::MissingHarness
            | Fault::Uncovered
            | Fault::Oversized
            | Fault::ChangedSource => Ok(()),
            #[cfg(unix)]
            Fault::LinkedResult | Fault::BlockedCleanup => Ok(()),
        }
    }
    #[cfg(unix)]
    pub fn perform_work(
        &mut self,
        command: &mut Command,
        budget: Duration,
    ) -> Result<(), ProcessError> {
        self.perform(command, budget, CommandMode::Work)
    }

    #[cfg(unix)]
    pub fn perform_cleanup(
        &mut self,
        command: &mut Command,
        budget: Duration,
    ) -> Result<(), ProcessError> {
        self.perform(command, budget, CommandMode::Cleanup)
    }

    fn close(self) -> io::Result<()> {
        if let Some(path) = self.temporary {
            if path.exists() {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let results = path.join("results");
                    if results.exists() {
                        std::fs::set_permissions(results, std::fs::Permissions::from_mode(0o755))?;
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
    let root = std::fs::canonicalize(std::env::temp_dir())?.join(format!(
        "rusterisk-proof-fixture-{}-{stamp}-{}",
        std::process::id(),
        std::panic::Location::caller().line()
    ));
    std::fs::create_dir(&root)?;
    let mut fixture = Fixture {
        root,
        fault: Fault::None,
        temporary: None,
        runs: 0,
        cleanups: 0,
    };
    let operation = (|| {
        let project = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .ok_or_else(|| io::Error::other("workspace missing"))?;
        CapturedCoreProofs::read(project)
            .map_err(io::Error::other)?
            .write(&fixture.root)
            .map_err(io::Error::other)?;
        test(&mut fixture)
    })();
    match (operation, fixture.close()) {
        (result, Ok(())) => result,
        (Ok(()), Err(error)) => Err(error),
        (Err(operation), Err(cleanup)) => Err(io::Error::other(format!(
            "{operation}; proof fixture cleanup failed: {cleanup}"
        ))),
    }
}
