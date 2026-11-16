use crate::{reference::ImageIdentity, reference::Step};
use std::{io, path::Path, time::Duration};

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
    Fetch,
    Build(Scope),
    Run(Scope),
}

impl Phase {
    pub fn label(self) -> String {
        match self {
            Self::Fetch => "fetch".to_owned(),
            Self::Build(scope) => format!("build-{}", scope.label()),
            Self::Run(scope) => format!("run-{}", scope.label()),
        }
    }
}

pub struct NativeSanitizerPlan {
    identity: String,
    source: String,
    state: String,
    image: ImageIdentity,
}

impl NativeSanitizerPlan {
    pub fn new(identity: &str, source: &str, state: &str, image: &str) -> io::Result<Self> {
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
        {
            return Err(invalid());
        }
        Ok(Self {
            identity: identity.into(),
            source: source.into(),
            state: state.into(),
            image,
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
            Phase::Fetch | Phase::Build(_) => 900,
            Phase::Run(_) => 180,
        };
        let fetch = phase == Phase::Fetch;
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
            "--env=RUSTFLAGS=-C linker=clang -C link-arg=-fsanitize=address,undefined",
            "--env=ASAN_OPTIONS=detect_leaks=1:halt_on_error=1:abort_on_error=1:detect_odr_violation=1",
            "--env=UBSAN_OPTIONS=halt_on_error=1:print_stacktrace=1",
            if fetch { "--env=CARGO_NET_OFFLINE=false" } else { "--env=CARGO_NET_OFFLINE=true" },
            "--entrypoint=/usr/bin/timeout", self.image.as_str(), "--signal=TERM", "--kill-after=10s",
            &format!("{}s", budget - 20), "cargo",
        ].into_iter().map(str::to_owned).collect();
        match phase {
            Phase::Fetch => arguments.extend(["fetch", "--locked"].map(str::to_owned)),
            Phase::Build(scope) | Phase::Run(scope) => {
                arguments.extend(["test", "--locked", "--offline"].map(str::to_owned));
                arguments.extend(
                    scope
                        .arguments()
                        .iter()
                        .map(|argument| (*argument).to_owned()),
                );
                match phase {
                    Phase::Build(_) => arguments.push("--no-run".into()),
                    Phase::Run(_) => {
                        if scope == Scope::Tls {
                            arguments.push("signaling::stream_tests::".into());
                        }
                        arguments.extend(
                            ["--", "--test-threads=1", "--format=pretty"].map(str::to_owned),
                        );
                    }
                    Phase::Fetch => {}
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
