use super::{
    callback_address,
    commands::{
        combine, create, docker, owned_step, read, write, CommandPhase, Commands, Observed,
        OUTPUT_LIMIT,
    },
    copy_configuration, media_address,
    resources::{reference_ready, reference_versions, start_reference, Reference},
    retained::{SippContext, SippRun},
    validate_sipp_statistics, Interop, ReferenceImage, SippAdapter,
};
use crate::{
    process::{CommandSupervisor, ProcessError, RunningCommand},
    server_distribution::{check_server_ready_log, ServerRuntimeConfig},
};
use std::{
    fs, io,
    net::{Ipv4Addr, SocketAddr, TcpListener, UdpSocket},
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
pub struct SippRegistrationComparison {
    asterisk: u64,
    rusterisk: u64,
    context: SippContext,
}

impl SippRegistrationComparison {
    pub(super) fn into_context(self) -> SippContext {
        self.context
    }
    pub const fn completed_calls(&self, adapter: SippAdapter) -> u64 {
        match adapter {
            SippAdapter::Asterisk => self.asterisk,
            SippAdapter::Rusterisk => self.rusterisk,
        }
    }
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
    ) -> io::Result<(u64, u8)> {
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
        let calls = validate_sipp_statistics(&observed.stdout).map_err(|error| {
            let adapter = match adapter {
                SippAdapter::Asterisk => "Asterisk",
                SippAdapter::Rusterisk => "Rusterisk",
            };
            io::Error::other(format!(
                "{adapter} SIPp statistics: {error}; {}",
                observed.diagnostics()
            ))
        })?;
        Ok((calls, commands.last_output()))
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
            .current_dir(self.root);
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

struct NativeObservation {
    calls: u64,
    statistics: u8,
    http: SocketAddr,
    sip: SocketAddr,
    version: u8,
    configuration: u8,
}

fn native_scenario(
    native: Native<'_>,
    commands: &mut Commands<'_>,
    supervisor: &mut CommandSupervisor,
    client: impl FnOnce(&mut RunningCommand<'_>, u16, &mut Commands<'_>) -> io::Result<(u64, u8)>,
) -> io::Result<NativeObservation> {
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
    let version = commands.last_output();
    let mut check = native.command();
    check.arg("--check-config");
    commands
        .run(check, Duration::from_secs(10), |command, budget| {
            supervisor.run(command, budget)
        })?
        .exact(b"Configuration valid.\n")?;
    let configuration = commands.last_output();
    let log = native.directory.join("native.stdout");
    let diagnostic = native.directory.join("native.stderr");
    let mut command = native.command();
    command
        .stdin(Stdio::null())
        .stdout(create(&log)?)
        .stderr(create(&diagnostic)?);
    drop(http_reservation);
    drop(sip_reservation);
    commands.scope(&mut command)?;
    let mut service = supervisor
        .start_service(&mut command, Duration::from_secs(90))
        .map_err(io::Error::other)?;
    let operation = (|| {
        native_ready(&mut service, &log, http)?;
        let (calls, statistics) = client(&mut service, sip.port(), commands)?;
        Ok(NativeObservation {
            calls,
            statistics,
            http,
            sip,
            version,
            configuration,
        })
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
    native: Native<'_>,
    plan: &Interop,
    selected: Option<&ReferenceImage>,
    reference: &mut Reference,
    commands: &mut Commands<'_>,
    supervisor: &mut CommandSupervisor,
    owner: &str,
) -> io::Result<(u64, u64, SippRun)> {
    let directory = commands.directory;
    let configuration = directory.join("asterisk");
    copy_configuration(&native.root.join("tests/interop/asterisk"), &configuration)?;
    fs::create_dir(configuration.join("sounds"))?;
    write(
        &configuration.join("sipp-registration.xml"),
        &read(
            &native.root.join("tests/interop/sipp/registration.xml"),
            65_536,
        )?,
    )?;
    let id = start_reference(reference, plan, selected, commands, supervisor)?;
    let tools = reference_versions(&id, commands, supervisor)?;
    reference_ready(&id, commands, supervisor)?;
    let step = owned_step(plan.media_address(), reference.name(), &id);
    let observed = commands.run(docker(&step.arguments), step.budget, |command, budget| {
        supervisor.run(command, budget)
    })?;
    let local = media_address(observed.text()?).map_err(io::Error::other)?;
    let local_output = commands.last_output();
    let step = owned_step(plan.callback_address(), reference.name(), &id);
    let observed = commands.run(docker(&step.arguments), step.budget, |command, budget| {
        supervisor.run(command, budget)
    })?;
    let callback = callback_address(observed.text()?).map_err(io::Error::other)?;
    let callback_output = commands.last_output();
    let scenario = Scenario {
        plan,
        name: reference.name(),
        id: &id,
        local,
    };
    let (asterisk, asterisk_statistics) = scenario.run(
        SippAdapter::Asterisk,
        SocketAddr::from((local, 5060)),
        commands,
        |command, budget| supervisor.run(command, budget),
    )?;
    fs::create_dir(native.directory)?;
    let rusterisk = native_scenario(native, commands, supervisor, |service, port, commands| {
        scenario.run(
            SippAdapter::Rusterisk,
            SocketAddr::from((callback, port)),
            commands,
            |command, budget| service.run(command, budget),
        )
    })?;
    Ok((
        asterisk,
        rusterisk.calls,
        SippRun {
            owner: owner.to_owned(),
            directory: directory.to_owned(),
            id,
            local,
            callback,
            local_output,
            callback_output,
            http: rusterisk.http,
            sip: rusterisk.sip,
            tools,
            statistics: [asterisk_statistics, rusterisk.statistics],
            version: rusterisk.version,
            configuration: rusterisk.configuration,
        },
    ))
}

pub fn compare_sipp_registrations_with(
    root: &Path,
    executable: &Path,
    supervisor: &mut CommandSupervisor,
) -> io::Result<SippRegistrationComparison> {
    compare(root, executable, None, supervisor)
}

impl ReferenceImage {
    pub(crate) fn compare_sipp_retained_with(
        &self,
        root: &Path,
        executable: &Path,
        supervisor: &mut CommandSupervisor,
        directory: &Path,
    ) -> io::Result<SippRegistrationComparison> {
        compare_in(root, executable, Some(self), supervisor, Some(directory))
    }
    pub fn compare_sipp_registrations_with(
        &self,
        root: &Path,
        executable: &Path,
        supervisor: &mut CommandSupervisor,
    ) -> io::Result<SippRegistrationComparison> {
        compare(root, executable, Some(self), supervisor)
    }
}

fn compare(
    root: &Path,
    executable: &Path,
    selected: Option<&ReferenceImage>,
    supervisor: &mut CommandSupervisor,
) -> io::Result<SippRegistrationComparison> {
    compare_in(root, executable, selected, supervisor, None)
}

fn compare_in(
    root: &Path,
    executable: &Path,
    selected: Option<&ReferenceImage>,
    supervisor: &mut CommandSupervisor,
    retained: Option<&Path>,
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
    let directory = match retained {
        Some(directory) => directory.to_owned(),
        None => fs::canonicalize(std::env::temp_dir())?.join(format!("rusterisk-sipp-{identity}")),
    };
    let configuration = directory.join("asterisk");
    let plan = Interop::new(
        &identity,
        configuration
            .to_str()
            .ok_or_else(|| io::Error::other("comparison path is not UTF-8"))?,
    )
    .map_err(io::Error::other)?;
    let reference = &mut Reference::new(&identity, directory.join("reference.cid"), &plan)?;
    fs::create_dir(&directory)?;
    let mut commands = match Commands::new(&root, &directory) {
        Ok(commands) => commands,
        Err(error) => {
            return combine(
                Err(error),
                fs::remove_dir_all(&directory),
                "comparison temporary cleanup",
            );
        }
    };
    let operation = run(
        Native {
            root: &root,
            executable: &executable,
            directory: &directory.join("native"),
        },
        &plan,
        selected,
        reference,
        &mut commands,
        supervisor,
        &identity,
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
    let cleanup = reference.cleanup_recorded(&mut commands, supervisor);
    let operation = match (operation, cleanup) {
        (Ok((asterisk, rusterisk, run)), Ok(Some(cleanup))) => Ok(SippRegistrationComparison {
            asterisk,
            rusterisk,
            context: SippContext { run, cleanup },
        }),
        (Ok(_), Ok(None)) => Err(io::Error::other(
            "completed SIPp comparison has no owned cleanup",
        )),
        (Err(primary), Ok(_)) | (Ok(_), Err(primary)) => Err(primary),
        (Err(primary), Err(cleanup)) => Err(io::Error::other(format!(
            "{primary}; reference cleanup: {cleanup}"
        ))),
    };
    match retained {
        Some(_) => operation,
        None => combine(
            operation,
            fs::remove_dir_all(&directory),
            "comparison temporary cleanup",
        ),
    }
}
