use crate::{reference::ImageIdentity, reference::Step};
use std::{io, path::Path, time::Duration};
mod results;
pub use results::{validate_rejection, Defect, NativeInventory};
mod archives;
pub use archives::{native_archives, validate_instrumented_archive, NativeArchive, NativeLibrary};
mod runner;
pub use runner::{CapturedNative, CommandMode, NativeError};
mod command;
#[cfg(unix)]
pub(crate) use command::retained::NativeExecution;
pub use command::{execute, plan, Execution, NativeOutcome};

const LLD_VERSION: &str = "LLD 23.1.5 (/checkout/src/llvm-project/llvm 0a5579eb649d3634a9c30ac873300cc2944fa070) (compatible with GNU linkers)";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    Media,
    Storage,
    Tls,
}

impl Scope {
    pub const ALL: [Self; 3] = [Self::Media, Self::Storage, Self::Tls];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Media => "media",
            Self::Storage => "storage",
            Self::Tls => "tls",
        }
    }

    fn arguments(self) -> &'static [&'static str] {
        match self {
            Self::Media => &[
                "--package",
                "rusterisk-media",
                "--test",
                "audio",
                "--test",
                "payload_decoder",
                "--test",
                "packet_duration",
            ],
            Self::Storage => &[
                "--package",
                "rusterisk-storage",
                "--test",
                "call_history",
                "--test",
                "recovery",
                "--test",
                "recording_replacement",
            ],
            Self::Tls => &["--package", "rusterisk-server", "--lib"],
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    ClangVersion,
    RustVersion,
    LldVersion,
    CompileProbe(Defect),
    RejectProbe(Defect),
    InspectArchive(NativeLibrary),
    Fetch,
    Build(Scope),
    Inventory(Scope),
    Run(Scope),
}

impl Phase {
    pub fn label(self) -> String {
        match self {
            Self::ClangVersion => "clang-version".to_owned(),
            Self::RustVersion => "rust-version".to_owned(),
            Self::LldVersion => "lld-version".to_owned(),
            Self::CompileProbe(defect) => format!("compile-probe-{}", defect.label()),
            Self::RejectProbe(defect) => format!("reject-probe-{}", defect.label()),
            Self::InspectArchive(library) => format!("inspect-{}", library.label()),
            Self::Fetch => "fetch".to_owned(),
            Self::Build(scope) => format!("build-{}", scope.label()),
            Self::Inventory(scope) => format!("inventory-{}", scope.label()),
            Self::Run(scope) => format!("run-{}", scope.label()),
        }
    }
}

impl Defect {
    pub const ALL: [Self; 2] = [Self::Address, Self::Undefined];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Address => "address",
            Self::Undefined => "undefined",
        }
    }

    pub const fn source(self) -> &'static str {
        match self {
            Self::Address => "#include <stdlib.h>\nint main(int argc, char **argv) {\n(void)argv;\nint *value = malloc(sizeof(int));\nif (!value) return 3;\n*value = argc;\nfree(value);\nreturn *(volatile int *)value;\n}\n",
            Self::Undefined => "int main(int argc, char **argv) {\n(void)argv;\nvolatile int value = 2147483647;\nvalue += argc;\nreturn value;\n}\n",
        }
    }
}

pub struct NativeSanitizerPlan {
    identity: String,
    source: String,
    state: String,
    image: ImageIdentity,
    linker: String,
}

impl NativeSanitizerPlan {
    pub fn image(&self) -> &str {
        self.image.as_str()
    }

    pub fn new(identity: &str, source: &str, state: &str, image: &str) -> io::Result<Self> {
        Self::for_architecture(identity, source, state, image, std::env::consts::ARCH)
    }

    pub(super) fn for_architecture(
        identity: &str,
        source: &str,
        state: &str,
        image: &str,
        architecture: &str,
    ) -> io::Result<Self> {
        let invalid = || io::Error::other("invalid native sanitizer plan");
        let image = ImageIdentity::parse(image).ok_or_else(invalid)?;
        if identity.is_empty()
            || identity.len() > 64
            || identity.starts_with('-')
            || !identity
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            || [source, state].iter().any(|path| {
                !Path::new(path).is_absolute()
                    || path.len() > 4096
                    || path.contains([',', '\r', '\n', '\0'])
                    || Path::new(path).components().any(|component| {
                        matches!(
                            component,
                            std::path::Component::ParentDir | std::path::Component::CurDir
                        )
                    })
            })
            || Path::new(source).starts_with(state)
            || Path::new(state).starts_with(source)
            || !matches!(architecture, "aarch64" | "x86_64")
        {
            return Err(invalid());
        }
        Ok(Self {
            identity: identity.into(),
            source: source.into(),
            state: state.into(),
            image,
            linker: format!("/usr/local/rustup/toolchains/1.100.0-{architecture}-unknown-linux-gnu/lib/rustlib/{architecture}-unknown-linux-gnu/bin/gcc-ld/ld.lld"),
        })
    }

    fn name(&self, phase: Phase) -> String {
        format!(
            "rusterisk-native-sanitizers-{}-{}",
            self.identity,
            phase.label()
        )
    }

    pub fn step(&self, phase: Phase) -> Step {
        let budget = match phase {
            Phase::ClangVersion
            | Phase::RustVersion
            | Phase::LldVersion
            | Phase::CompileProbe(_)
            | Phase::RejectProbe(_) => 50,
            Phase::InspectArchive(_) => 30,
            Phase::Fetch | Phase::Build(_) => 900,
            Phase::Inventory(_) | Phase::Run(_) => 180,
        };
        let fetch = phase == Phase::Fetch;
        let exit_code = if phase == Phase::RejectProbe(Defect::Undefined) {
            87
        } else {
            86
        };
        let mut arguments: Vec<String> = [
            "run", "--rm", "--pull=never", "--name", &self.name(phase),
            "--cpus=1", "--memory=2g", "--memory-swap=2g", "--pids-limit=256",
            if fetch { "--network=bridge" } else { "--network=none" },
            "--read-only", "--cap-drop=ALL", "--security-opt=no-new-privileges",
            "--ulimit", "fsize=536870912:536870912", "--tmpfs", "/tmp:mode=1777,size=128m",
            "--mount", &format!("type=bind,source={},target=/project,readonly", self.source),
            "--mount", &format!("type=bind,source={},target=/state", self.state),
            "--workdir=/project", "--env=CARGO_HOME=/state/cargo", "--env=CARGO_TARGET_DIR=/state/target",
            "--env=CARGO_BUILD_JOBS=1", "--env=CARGO_PROFILE_DEV_DEBUG=line-tables-only",
            "--env=CARGO_PROFILE_DEV_INCREMENTAL=false", "--env=TMPDIR=/state/tmp",
            "--env=CC=clang", "--env=CXX=clang++", "--env=AWS_LC_SYS_SANITIZER=asan",
            "--env=CFLAGS=-fsanitize=address,undefined -fno-sanitize-recover=all -fno-omit-frame-pointer -g",
            "--env=CXXFLAGS=-fsanitize=address,undefined -fno-sanitize-recover=all -fno-omit-frame-pointer -g",
            &format!("--env=RUSTFLAGS=-C linker=clang -C link-arg=-fsanitize=address,undefined -C link-arg=--ld-path={} -C link-arg=-Wl,--threads=1", self.linker),
            "--env=ASAN_SYMBOLIZER_PATH=/usr/bin/llvm-symbolizer-14",
            &format!("--env=ASAN_OPTIONS=detect_leaks=1:halt_on_error=1:abort_on_error=0:exitcode={exit_code}:detect_odr_violation=1"),
            &format!("--env=UBSAN_OPTIONS=halt_on_error=1:print_stacktrace=1:exitcode={exit_code}"),
            if fetch { "--env=CARGO_NET_OFFLINE=false" } else { "--env=CARGO_NET_OFFLINE=true" },
            "--entrypoint=/usr/bin/timeout", self.image.as_str(), "--signal=TERM", "--kill-after=10s",
            &format!("{}s", budget - 20),
        ].into_iter().map(str::to_owned).collect();
        match phase {
            Phase::ClangVersion => arguments.extend(["clang", "--version"].map(str::to_owned)),
            Phase::RustVersion => arguments.extend(["rustc", "-vV"].map(str::to_owned)),
            Phase::LldVersion => arguments.extend([self.linker.clone(), "--version".into()]),
            Phase::CompileProbe(defect) => arguments.extend([
                "clang".to_owned(),
                format!("--ld-path={}", self.linker),
                "-Wl,--threads=1".to_owned(),
                "-O1".to_owned(),
                "-g".to_owned(),
                "-fsanitize=address,undefined".to_owned(),
                "-fno-sanitize-recover=all".to_owned(),
                "-fno-omit-frame-pointer".to_owned(),
                format!("/state/probe-{}.c", defect.label()),
                "-o".to_owned(),
                format!("/state/probe-{}", defect.label()),
            ]),
            Phase::RejectProbe(defect) => {
                arguments.push(format!("/state/probe-{}", defect.label()))
            }
            Phase::InspectArchive(library) => arguments.extend([
                "nm".to_owned(),
                "--undefined-only".to_owned(),
                format!("/state/native/lib-{}.a", library.label()),
            ]),
            Phase::Fetch => arguments.extend(["cargo", "fetch", "--locked"].map(str::to_owned)),
            Phase::Build(scope) | Phase::Inventory(scope) | Phase::Run(scope) => {
                arguments.extend(["cargo", "test", "--locked", "--offline"].map(str::to_owned));
                arguments.extend(
                    scope
                        .arguments()
                        .iter()
                        .map(|argument| (*argument).to_owned()),
                );
                match phase {
                    Phase::Build(_) => arguments.extend([
                        "--no-run".to_owned(),
                        "--message-format=json-render-diagnostics".to_owned(),
                    ]),
                    Phase::Inventory(_) | Phase::Run(_) => {
                        if scope == Scope::Tls {
                            arguments.push("signaling::stream_tests::".into());
                        }
                        arguments.extend(
                            ["--", "--test-threads=1", "--format=pretty"].map(str::to_owned),
                        );
                        if matches!(phase, Phase::Inventory(_)) {
                            arguments.push("--list".into());
                        }
                    }
                    Phase::ClangVersion
                    | Phase::RustVersion
                    | Phase::LldVersion
                    | Phase::CompileProbe(_)
                    | Phase::RejectProbe(_)
                    | Phase::InspectArchive(_)
                    | Phase::Fetch => {}
                }
            }
        }
        Step {
            arguments,
            budget: Duration::from_secs(budget),
        }
    }

    pub fn cleanup(&self, phase: Phase) -> Step {
        Step {
            arguments: ["rm", "--force", &self.name(phase)]
                .map(str::to_owned)
                .into(),
            budget: Duration::from_secs(30),
        }
    }
}
