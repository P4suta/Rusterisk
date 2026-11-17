use super::{
    callback_address, copy_configuration, media_address, validate_sipp_statistics, ImageIdentity,
    Interop, SippAdapter, Step,
};
use crate::{
    process::{CommandSupervisor, ProcessError, RunningCommand},
    server_distribution::{check_server_ready_log, ServerRuntimeConfig},
};
use serde::Deserialize;
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    net::{Ipv4Addr, SocketAddr, TcpListener, UdpSocket},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const REFERENCE_IMAGE: &str = "rusterisk-asterisk-reference:22.9.0";
const OWNERSHIP_LABEL: &str = "io.rusterisk.sipp-comparison";
const OUTPUT_LIMIT: u64 = 1024 * 1024;

pub struct SippRegistrationComparison {
    asterisk: u64,
    rusterisk: u64,
}

impl SippRegistrationComparison {
    pub const fn completed_calls(&self, adapter: SippAdapter) -> u64 {
        match adapter {
            SippAdapter::Asterisk => self.asterisk,
            SippAdapter::Rusterisk => self.rusterisk,
        }
    }
}

fn combine<T>(operation: io::Result<T>, cleanup: io::Result<()>, context: &str) -> io::Result<T> {
    match (operation, cleanup) {
        (operation, Ok(())) => operation,
        (Ok(_), Err(error)) => Err(error),
        (Err(operation), Err(cleanup)) => Err(io::Error::other(format!(
            "{operation}; {context}: {cleanup}"
        ))),
    }
}

fn read(path: &Path, limit: u64) -> io::Result<Vec<u8>> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.len() > limit {
        return Err(io::Error::other("comparison output exceeds its file limit"));
    }
    let mut bytes = Vec::new();
    File::open(path)?.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(io::Error::other(
            "comparison output exceeded its byte limit",
        ));
    }
    Ok(bytes)
}

fn create(path: &Path) -> io::Result<File> {
    OpenOptions::new().write(true).create_new(true).open(path)
}

fn write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = create(path)?;
    file.write_all(bytes)?;
    file.flush()
}

struct Observed {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

impl Observed {
    fn diagnostics(&self) -> String {
        format!(
            "stdout: {}; stderr: {}",
            String::from_utf8_lossy(&self.stdout[..self.stdout.len().min(4096)]),
            String::from_utf8_lossy(&self.stderr[..self.stderr.len().min(4096)]),
        )
    }

    fn exact(&self, expected: &[u8]) -> io::Result<()> {
        if self.stdout != expected || !self.stderr.is_empty() {
            return Err(io::Error::other(format!(
                "comparison command output changed; {}",
                self.diagnostics()
            )));
        }
        Ok(())
    }

    fn text(&self) -> io::Result<&str> {
        if !self.stderr.is_empty() {
            return Err(io::Error::other(format!(
                "comparison inspection produced diagnostics; {}",
                self.diagnostics()
            )));
        }
        std::str::from_utf8(&self.stdout).map_err(io::Error::other)
    }
}

struct Commands<'a> {
    directory: &'a Path,
    sequence: u8,
    phase: CommandPhase,
}

enum CommandPhase {
    Operation,
    Cleanup,
}

impl Commands<'_> {
    fn run(
        &mut self,
        mut command: Command,
        budget: Duration,
        perform: impl FnOnce(&mut Command, Duration) -> Result<(), ProcessError>,
    ) -> io::Result<Observed> {
        let limit = match self.phase {
            CommandPhase::Operation => 128,
            CommandPhase::Cleanup => 144,
        };
        self.sequence = self
            .sequence
            .checked_add(1)
            .filter(|sequence| *sequence <= limit)
            .ok_or_else(|| io::Error::other("comparison command capacity exhausted"))?;
        let stdout = self.directory.join(format!("{:03}.stdout", self.sequence));
        let stderr = self.directory.join(format!("{:03}.stderr", self.sequence));
        command
            .stdin(Stdio::null())
            .stdout(create(&stdout)?)
            .stderr(create(&stderr)?);
        let operation = perform(&mut command, budget);
        let observed = (|| {
            Ok(Observed {
                stdout: read(&stdout, OUTPUT_LIMIT)?,
                stderr: read(&stderr, OUTPUT_LIMIT)?,
            })
        })();
        match (operation, observed) {
            (Ok(()), Ok(observed)) => Ok(observed),
            (Err(operation), Ok(observed)) => Err(io::Error::other(format!(
                "{operation}; {}",
                observed.diagnostics()
            ))),
            (Ok(()), Err(error)) => Err(error),
            (Err(operation), Err(cleanup)) => Err(io::Error::other(format!(
                "{operation}; comparison log preservation failed: {cleanup}"
            ))),
        }
    }
}

fn docker(arguments: &[String]) -> Command {
    let mut command = Command::new("docker");
    command.args(arguments);
    command
}

fn arguments<const N: usize>(values: [&str; N]) -> Vec<String> {
    values.into_iter().map(str::to_owned).collect()
}

fn owned_step(mut step: Step, name: &str, id: &str) -> Step {
    for argument in &mut step.arguments {
        if argument == name {
            *argument = id.into();
        }
    }
    step
}

fn container_id(bytes: &[u8]) -> io::Result<&str> {
    let source = std::str::from_utf8(bytes).map_err(io::Error::other)?;
    let id = source.strip_suffix('\n').unwrap_or(source);
    if id.len() != 64
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Err(io::Error::other("comparison container ID is invalid"));
    }
    Ok(id)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Ownership {
    id: String,
    requested_image: String,
    name: String,
    owner: Option<String>,
}

struct Located {
    id: Option<String>,
    damage: Option<io::Error>,
}

fn locate_container(
    cidfile: io::Result<Vec<u8>>,
    attempted: bool,
    lookup: impl FnOnce() -> io::Result<Vec<u8>>,
) -> io::Result<Located> {
    let damage = match cidfile.and_then(|bytes| container_id(&bytes).map(str::to_owned)) {
        Ok(id) => {
            return Ok(Located {
                id: Some(id),
                damage: None,
            })
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => Some(error),
    };
    if !attempted {
        return Ok(Located { id: None, damage });
    }
    let selected = lookup().and_then(|bytes| {
        if bytes.is_empty() {
            return Ok(None);
        }
        container_id(&bytes).map(|id| Some(id.to_owned()))
    });
    match selected {
        Ok(id) => Ok(Located { id, damage }),
        Err(error) => combine(
            Err(error),
            damage.map_or(Ok(()), Err),
            "comparison CID recovery",
        ),
    }
}

struct Reference {
    name: String,
    owner: String,
    cidfile: PathBuf,
    image: Option<String>,
    attempted: bool,
}

impl Reference {
    fn check(&self, id: &str, ownership: Ownership) -> io::Result<String> {
        if ownership.id != id
            || self.image.as_deref() != Some(ownership.requested_image.as_str())
            || ownership.name != format!("/{}", self.name)
            || ownership.owner.as_deref() != Some(self.owner.as_str())
        {
            return Err(io::Error::other(
                "comparison container ownership differs; control refused",
            ));
        }
        Ok(ownership.id)
    }

    fn inspect(
        &self,
        id: &str,
        commands: &mut Commands<'_>,
        perform: impl FnOnce(&mut Command, Duration) -> Result<(), ProcessError>,
    ) -> io::Result<String> {
        let format = format!(
            "{{\"id\":{{{{json .Id}}}},\"requested_image\":{{{{json .Config.Image}}}},\"name\":{{{{json .Name}}}},\"owner\":{{{{json (index .Config.Labels \"{OWNERSHIP_LABEL}\")}}}}}}"
        );
        let observed = commands.run(
            docker(&arguments([
                "container",
                "inspect",
                "--format",
                &format,
                id,
            ])),
            Duration::from_secs(10),
            perform,
        )?;
        let ownership: Ownership =
            serde_json::from_str(observed.text()?).map_err(io::Error::other)?;
        self.check(id, ownership)
    }

    fn locate(
        &self,
        commands: &mut Commands<'_>,
        supervisor: &mut CommandSupervisor,
    ) -> io::Result<Located> {
        let filter = format!("label={OWNERSHIP_LABEL}={}", self.owner);
        locate_container(read(&self.cidfile, 65), self.attempted, || {
            let observed = commands.run(
                docker(&arguments([
                    "container",
                    "ls",
                    "--all",
                    "--no-trunc",
                    "--filter",
                    &filter,
                    "--format",
                    "{{.ID}}",
                ])),
                Duration::from_secs(10),
                |command, budget| supervisor.run_cleanup(command, budget),
            )?;
            observed.text()?;
            Ok(observed.stdout)
        })
    }

    fn cleanup(
        &self,
        commands: &mut Commands<'_>,
        supervisor: &mut CommandSupervisor,
    ) -> io::Result<()> {
        let located = self.locate(commands, supervisor)?;
        let operation = (|| {
            let Some(id) = located.id else {
                return Ok(());
            };
            let id = self.inspect(&id, commands, |command, budget| {
                supervisor.run_cleanup(command, budget)
            })?;
            commands.run(
                docker(&arguments(["rm", "--force", &id])),
                Duration::from_secs(30),
                |command, budget| supervisor.run_cleanup(command, budget),
            )?;
            let filter = format!("id={id}");
            commands
                .run(
                    docker(&arguments([
                        "container",
                        "ls",
                        "--all",
                        "--filter",
                        &filter,
                        "--format",
                        "{{.ID}}",
                    ])),
                    Duration::from_secs(10),
                    |command, budget| supervisor.run_cleanup(command, budget),
                )?
                .exact(b"")
        })();
        combine(
            operation,
            located.damage.map_or(Ok(()), Err),
            "comparison CID recovery",
        )
    }

    fn diagnostics(
        &self,
        plan: &Interop,
        commands: &mut Commands<'_>,
        supervisor: &mut CommandSupervisor,
    ) -> io::Result<String> {
        let located = self.locate(commands, supervisor)?;
        let observed = (|| {
            let Some(id) = located.id else {
                return Ok("No comparison container was created".to_owned());
            };
            let id = self.inspect(&id, commands, |command, budget| {
                supervisor.run_cleanup(command, budget)
            })?;
            let step = owned_step(plan.logs(), &self.name, &id);
            commands
                .run(docker(&step.arguments), step.budget, |command, budget| {
                    supervisor.run_cleanup(command, budget)
                })
                .map(|observed| observed.diagnostics())
        })();
        match (observed, located.damage) {
            (Ok(observed), None) => Ok(observed),
            (Ok(observed), Some(damage)) => Ok(format!("CID recovery: {damage}; {observed}")),
            (Err(error), damage) => combine(
                Err(error),
                damage.map_or(Ok(()), Err),
                "comparison CID recovery",
            ),
        }
    }
}

fn start_reference(
    reference: &mut Reference,
    plan: &Interop,
    commands: &mut Commands<'_>,
    supervisor: &mut CommandSupervisor,
) -> io::Result<String> {
    let inspected = commands.run(
        docker(&arguments([
            "image",
            "inspect",
            "--format",
            "{{.Id}}",
            REFERENCE_IMAGE,
        ])),
        Duration::from_secs(10),
        |command, budget| supervisor.run(command, budget),
    )?;
    let source = inspected.text()?;
    let source = source.strip_suffix('\n').unwrap_or(source);
    let image = ImageIdentity::parse(source)
        .filter(|image| image.as_str().starts_with("sha256:"))
        .ok_or_else(|| io::Error::other("comparison reference image ID is invalid"))?;
    reference.image = Some(image.as_str().into());
    let mut start = plan.start();
    let mut replacements = 0;
    for argument in &mut start.arguments {
        if argument == REFERENCE_IMAGE {
            *argument = image.as_str().into();
            replacements += 1;
        }
    }
    if replacements != 1 {
        return Err(io::Error::other(
            "comparison reference image selection changed",
        ));
    }
    let cidfile = reference
        .cidfile
        .to_str()
        .ok_or_else(|| io::Error::other("comparison CID path is not UTF-8"))?;
    for value in [
        cidfile.to_owned(),
        "--cidfile".into(),
        format!("{OWNERSHIP_LABEL}={}", reference.owner),
        "--label".into(),
    ] {
        start.arguments.insert(1, value);
    }
    reference.attempted = true;
    commands.run(docker(&start.arguments), start.budget, |command, budget| {
        supervisor.run(command, budget)
    })?;
    let bytes = read(&reference.cidfile, 65)?;
    reference.inspect(container_id(&bytes)?, commands, |command, budget| {
        supervisor.run(command, budget)
    })
}

fn reference_versions(
    id: &str,
    commands: &mut Commands<'_>,
    supervisor: &mut CommandSupervisor,
) -> io::Result<()> {
    commands
        .run(
            docker(&arguments(["exec", id, "/usr/sbin/asterisk", "-V"])),
            Duration::from_secs(10),
            |command, budget| supervisor.run(command, budget),
        )?
        .exact(b"Asterisk 22.9.0\n")?;
    let observed = commands.run(
        docker(&arguments(["exec", id, "/usr/local/bin/sipp", "-v"])),
        Duration::from_secs(10),
        |command, budget| match supervisor.run(command, budget) {
            Err(ProcessError::Status(status)) if status.code() == Some(99) => Ok(()),
            Ok(()) => Err(ProcessError::InvalidOutput),
            Err(error) => Err(error),
        },
    )?;
    if observed
        .text()?
        .lines()
        .filter(|line| *line == " SIPp v3.8.0-TLS.")
        .count()
        != 1
    {
        return Err(io::Error::other(format!(
            "comparison SIPp version differs; {}",
            observed.diagnostics()
        )));
    }
    Ok(())
}

fn reference_ready(
    id: &str,
    commands: &mut Commands<'_>,
    supervisor: &mut CommandSupervisor,
) -> io::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let mut ready = false;
        commands
            .run(
                docker(&arguments([
                    "exec",
                    id,
                    "/usr/bin/test",
                    "-S",
                    "/var/run/asterisk/asterisk.ctl",
                ])),
                deadline.saturating_duration_since(Instant::now()),
                |command, budget| match supervisor.run(command, budget) {
                    Ok(()) => {
                        ready = true;
                        Ok(())
                    }
                    Err(ProcessError::Status(status)) if status.code() == Some(1) => Ok(()),
                    Err(error) => Err(error),
                },
            )?
            .exact(b"")?;
        if ready {
            break;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::Error::other(
                "comparison Asterisk control socket was not ready",
            ));
        }
        std::thread::sleep(remaining.min(Duration::from_millis(100)));
    }
    let observed = commands.run(
        docker(&arguments([
            "exec",
            id,
            "/usr/bin/timeout",
            "--kill-after=2s",
            "10s",
            "/usr/sbin/asterisk",
            "-rx",
            "core waitfullybooted",
        ])),
        Duration::from_secs(15),
        |command, budget| supervisor.run(command, budget),
    )?;
    if !observed.text()?.contains("Asterisk has fully booted.\n") {
        return Err(io::Error::other(format!(
            "comparison Asterisk did not finish startup; {}",
            observed.diagnostics()
        )));
    }
    Ok(())
}

struct Scenario<'a> {
    plan: &'a Interop,
    name: &'a str,
    id: &'a str,
    local: Ipv4Addr,
}

impl Scenario<'_> {
    fn run(
        &self,
        adapter: SippAdapter,
        target: SocketAddr,
        commands: &mut Commands<'_>,
        mut perform: impl FnMut(&mut Command, Duration) -> Result<(), ProcessError>,
    ) -> io::Result<u64> {
        let step = owned_step(
            self.plan
                .sipp_registration(adapter, target, self.local)
                .map_err(io::Error::other)?,
            self.name,
            self.id,
        );
        commands.run(docker(&step.arguments), step.budget, &mut perform)?;
        let statistics = owned_step(self.plan.sipp_statistics(adapter), self.name, self.id);
        let observed = commands.run(docker(&statistics.arguments), statistics.budget, perform)?;
        if !observed.stderr.is_empty() {
            return Err(io::Error::other(format!(
                "comparison statistics produced diagnostics; {}",
                observed.diagnostics()
            )));
        }
        validate_sipp_statistics(&observed.stdout).map_err(io::Error::other)
    }
}

struct Native<'a> {
    root: &'a Path,
    executable: &'a Path,
    directory: &'a Path,
}

impl Native<'_> {
    fn prepare(&self, http: SocketAddr, sip_port: u16) -> io::Result<()> {
        let configuration =
            ServerRuntimeConfig::for_registration_comparison(http, sip_port, |path| {
                read(&self.root.join(path), 65_536)
            })?;
        fs::create_dir(self.directory.join("config"))?;
        fs::create_dir(self.directory.join("data"))?;
        for (path, bytes) in configuration.files() {
            write(&self.directory.join(path), bytes)?;
        }
        Ok(())
    }

    fn command(&self) -> Command {
        let mut command = Command::new(self.executable);
        command
            .arg("--config-dir")
            .arg(self.directory.join("config"))
            .arg("--data-dir")
            .arg(self.directory.join("data"))
            .current_dir(self.directory);
        command
    }
}

fn native_ready(service: &mut RunningCommand<'_>, log: &Path, http: SocketAddr) -> io::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        service.poll().map_err(io::Error::other)?;
        if check_server_ready_log(&read(log, OUTPUT_LIMIT)?, http)? {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(io::Error::other(
                "comparison native server readiness expired",
            ));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn native_scenario(
    native: Native<'_>,
    commands: &mut Commands<'_>,
    supervisor: &mut CommandSupervisor,
    client: impl FnOnce(&mut RunningCommand<'_>, u16, &mut Commands<'_>) -> io::Result<u64>,
) -> io::Result<u64> {
    let http_reservation = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    let sip_reservation = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))?;
    let http = http_reservation.local_addr()?;
    let sip = sip_reservation.local_addr()?;
    native.prepare(http, sip.port())?;
    let mut version = Command::new(native.executable);
    version.arg("--version");
    commands
        .run(version, Duration::from_secs(10), |command, budget| {
            supervisor.run(command, budget)
        })?
        .exact(format!("rusterisk {}\n", env!("CARGO_PKG_VERSION")).as_bytes())?;
    let mut check = native.command();
    check.arg("--check-config");
    commands
        .run(check, Duration::from_secs(10), |command, budget| {
            supervisor.run(command, budget)
        })?
        .exact(b"Configuration valid.\n")?;
    let log = native.directory.join("native.stdout");
    let diagnostic = native.directory.join("native.stderr");
    let mut command = native.command();
    command
        .stdin(Stdio::null())
        .stdout(create(&log)?)
        .stderr(create(&diagnostic)?);
    drop(http_reservation);
    drop(sip_reservation);
    let mut service = supervisor
        .start_service(&mut command, Duration::from_secs(90))
        .map_err(io::Error::other)?;
    let operation = (|| {
        native_ready(&mut service, &log, http)?;
        client(&mut service, sip.port(), commands)
    })();
    let operation = combine(
        operation,
        service.stop().map_err(io::Error::other),
        "native shutdown",
    );
    let diagnostics = (|| {
        let observed = Observed {
            stdout: read(&log, OUTPUT_LIMIT)?,
            stderr: read(&diagnostic, OUTPUT_LIMIT)?,
        };
        if !observed.stderr.is_empty() || !check_server_ready_log(&observed.stdout, http)? {
            return Err(io::Error::other(format!(
                "comparison native server diagnostics changed; {}",
                observed.diagnostics()
            )));
        }
        let _http = TcpListener::bind(http)?;
        let _sip = UdpSocket::bind(sip)?;
        Ok(())
    })();
    combine(
        operation,
        diagnostics,
        "native diagnostics and listener cleanup",
    )
}

fn run(
    root: &Path,
    executable: &Path,
    directory: &Path,
    plan: &Interop,
    reference: &mut Reference,
    commands: &mut Commands<'_>,
    supervisor: &mut CommandSupervisor,
) -> io::Result<SippRegistrationComparison> {
    let configuration = directory.join("asterisk");
    copy_configuration(&root.join("tests/interop/asterisk"), &configuration)?;
    fs::create_dir(configuration.join("sounds"))?;
    write(
        &configuration.join("sipp-registration.xml"),
        &read(&root.join("tests/interop/sipp/registration.xml"), 65_536)?,
    )?;
    let id = start_reference(reference, plan, commands, supervisor)?;
    reference_versions(&id, commands, supervisor)?;
    reference_ready(&id, commands, supervisor)?;
    let step = owned_step(plan.media_address(), &reference.name, &id);
    let observed = commands.run(docker(&step.arguments), step.budget, |command, budget| {
        supervisor.run(command, budget)
    })?;
    let local = media_address(observed.text()?).map_err(io::Error::other)?;
    let step = owned_step(plan.callback_address(), &reference.name, &id);
    let observed = commands.run(docker(&step.arguments), step.budget, |command, budget| {
        supervisor.run(command, budget)
    })?;
    let callback = callback_address(observed.text()?).map_err(io::Error::other)?;
    let scenario = Scenario {
        plan,
        name: &reference.name,
        id: &id,
        local,
    };
    let asterisk = scenario.run(
        SippAdapter::Asterisk,
        SocketAddr::from((local, 5060)),
        commands,
        |command, budget| supervisor.run(command, budget),
    )?;
    let runtime = directory.join("native");
    fs::create_dir(&runtime)?;
    let rusterisk = native_scenario(
        Native {
            root,
            executable,
            directory: &runtime,
        },
        commands,
        supervisor,
        |service, port, commands| {
            scenario.run(
                SippAdapter::Rusterisk,
                SocketAddr::from((callback, port)),
                commands,
                |command, budget| service.run(command, budget),
            )
        },
    )?;
    Ok(SippRegistrationComparison {
        asterisk,
        rusterisk,
    })
}

pub fn compare_sipp_registrations_with(
    root: &Path,
    executable: &Path,
    supervisor: &mut CommandSupervisor,
) -> io::Result<SippRegistrationComparison> {
    let root = fs::canonicalize(root)?;
    let executable = fs::canonicalize(executable)?;
    if !executable.is_file() {
        return Err(io::Error::other(
            "comparison native server is not a regular file",
        ));
    }
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    let identity = format!("{}-{stamp}", std::process::id());
    let directory =
        fs::canonicalize(std::env::temp_dir())?.join(format!("rusterisk-sipp-{identity}"));
    let configuration = directory.join("asterisk");
    let plan = Interop::new(
        &identity,
        configuration
            .to_str()
            .ok_or_else(|| io::Error::other("comparison path is not UTF-8"))?,
    )
    .map_err(io::Error::other)?;
    let start = plan.start();
    let name = start
        .arguments
        .windows(2)
        .find(|parts| parts[0] == "--name")
        .map(|parts| parts[1].clone())
        .ok_or_else(|| io::Error::other("comparison container name missing"))?;
    fs::create_dir(&directory)?;
    let mut commands = Commands {
        directory: &directory,
        sequence: 0,
        phase: CommandPhase::Operation,
    };
    let reference = &mut Reference {
        name,
        owner: identity,
        cidfile: directory.join("reference.cid"),
        image: None,
        attempted: false,
    };
    let operation = run(
        &root,
        &executable,
        &directory,
        &plan,
        reference,
        &mut commands,
        supervisor,
    );
    commands.phase = CommandPhase::Cleanup;
    let operation = if operation.is_err() {
        let diagnostics = reference.diagnostics(&plan, &mut commands, supervisor);
        match (operation, diagnostics) {
            (Err(error), Ok(observed)) => Err(io::Error::other(format!(
                "{error}; reference diagnostics: {}",
                observed
            ))),
            (operation, Err(error)) => {
                combine(operation, Err(error), "reference diagnostic preservation")
            }
            (Ok(comparison), Ok(_)) => Ok(comparison),
        }
    } else {
        operation
    };
    let operation = combine(
        operation,
        reference.cleanup(&mut commands, supervisor),
        "reference cleanup",
    );
    combine(
        operation,
        fs::remove_dir_all(&directory),
        "comparison temporary cleanup",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incomplete_cid_files_recover_the_unique_label_id_and_keep_the_damage() -> io::Result<()> {
        let id = "a".repeat(64);
        for bytes in [Vec::new(), b"partly-written".to_vec(), vec![b'a'; 63]] {
            let mut queried = false;
            let located = locate_container(Ok(bytes), true, || {
                queried = true;
                Ok(format!("{id}\n").into_bytes())
            })?;
            assert!(queried);
            assert_eq!(located.id.as_deref(), Some(id.as_str()));
            assert!(located.damage.is_some());
        }
        let located = locate_container(
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "CID read failed",
            )),
            true,
            || Ok(format!("{id}\n").into_bytes()),
        )?;
        assert_eq!(located.id.as_deref(), Some(id.as_str()));
        assert!(located
            .damage
            .is_some_and(|damage| damage.to_string() == "CID read failed"));
        Ok(())
    }

    #[test]
    fn an_owned_cid_does_not_search_names_and_absent_unstarted_resources_stay_absent(
    ) -> io::Result<()> {
        let id = "a".repeat(64);
        let located = locate_container(Ok(id.as_bytes().to_vec()), true, || {
            Err(io::Error::other(
                "an exact CID must not select another container",
            ))
        })?;
        assert_eq!(located.id.as_deref(), Some(id.as_str()));
        assert!(located.damage.is_none());
        let located = locate_container(
            Err(io::Error::new(io::ErrorKind::NotFound, "no CID")),
            false,
            || {
                Err(io::Error::other(
                    "an unstarted comparison must not select containers",
                ))
            },
        )?;
        assert!(located.id.is_none() && located.damage.is_none());
        Ok(())
    }

    #[test]
    fn label_recovery_rejects_ambiguous_ids_and_preserves_a_failed_lookup() -> io::Result<()> {
        let first = "a".repeat(64);
        let second = "b".repeat(64);
        for bytes in [
            format!("{first}\n{second}\n").into_bytes(),
            format!("{first}\n\n").into_bytes(),
            b"foreign-container-name\n".to_vec(),
        ] {
            assert!(locate_container(Ok(Vec::new()), true, || Ok(bytes)).is_err());
        }
        let error = match locate_container(Ok(Vec::new()), true, || {
            Err(io::Error::other("label lookup failed"))
        }) {
            Err(error) => error,
            Ok(_) => return Err(io::Error::other("failed label lookup passed")),
        };
        assert!(error.to_string().contains("label lookup failed"));
        assert!(error.to_string().contains("container ID is invalid"));
        Ok(())
    }

    #[test]
    fn container_control_requires_its_id_label_name_and_requested_image() -> io::Result<()> {
        let id = "a".repeat(64);
        let image = format!("sha256:{}", "b".repeat(64));
        let reference = Reference {
            name: "rusterisk-interop-fixture".into(),
            owner: "fixture".into(),
            cidfile: PathBuf::from("unused.cid"),
            image: Some(image.clone()),
            attempted: true,
        };
        let ownership = || Ownership {
            id: id.clone(),
            requested_image: image.clone(),
            name: "/rusterisk-interop-fixture".into(),
            owner: Some("fixture".into()),
        };
        assert_eq!(reference.check(&id, ownership())?, id);
        for field in ["id", "label", "name", "requested image", "missing label"] {
            let mut changed = ownership();
            match field {
                "id" => changed.id = "c".repeat(64),
                "label" => changed.owner = Some("foreign".into()),
                "name" => changed.name = "/foreign".into(),
                "requested image" => changed.requested_image = format!("sha256:{}", "c".repeat(64)),
                "missing label" => changed.owner = None,
                _ => return Err(io::Error::other("unknown identity fixture")),
            }
            assert!(reference.check(&id, changed).is_err());
        }
        Ok(())
    }
}
