use super::{
    docker::Docker,
    identity::{Descriptor, EngineImage, ExpectedImage, INSPECT},
    CommandSupervisor, ServerOciRunPhase, ServerOciRuntimePlan,
};
use crate::{
    process::{ProcessError, RunningCommand},
    server_distribution::{
        check_server_ready_log,
        native::{combine, write},
        runtime_commands::{RuntimeCommands, RuntimeService},
    },
};
use serde::Deserialize;
use std::{
    io,
    net::{SocketAddr, TcpListener, UdpSocket},
    path::Path,
    process::Command,
    time::{Duration, Instant},
};

pub(super) struct RuntimeObservation {
    pub http: SocketAddr,
    pub sip: SocketAddr,
    pub profile: super::identity::OciEngineProfile,
}

pub(super) struct Runtime<'a> {
    pub identity: &'a str,
    pub configuration: &'a Path,
    pub expected: ExpectedImage<'a>,
    pub archive: &'a Path,
    pub version: &'a str,
    pub artifacts: &'a Path,
    pub http_reservation: Option<TcpListener>,
    pub sip_reservation: Option<UdpSocket>,
}

struct ActiveRuntime<'a> {
    pub plan: ServerOciRuntimePlan,
    pub image: EngineImage<'a>,
    pub version: &'a str,
    pub http_reservation: Option<TcpListener>,
    pub sip_reservation: Option<UdpSocket>,
}

fn arguments<const N: usize>(parts: [&str; N]) -> Vec<String> {
    parts.into_iter().map(str::to_owned).collect()
}

fn present<F, G>(
    docker: &mut Docker<'_, F>,
    supervisor: &mut RuntimeCommands<'_, G>,
    name: &str,
    cleanup: bool,
) -> io::Result<bool>
where
    F: FnMut(&[String], &Path, &Path) -> io::Result<Command>,
    G: FnMut(&mut Command, Duration) -> io::Result<()>,
{
    let filter = format!("name=^/{name}$");
    let observed = docker.run(
        supervisor,
        &arguments([
            "container",
            "ls",
            "--all",
            "--filter",
            &filter,
            "--format",
            "{{.Names}}",
        ]),
        Duration::from_secs(15),
        "containers",
        cleanup,
    )?;
    container_present(&observed.stdout, &observed.stderr, name)
}

pub(super) fn container_present(stdout: &[u8], stderr: &[u8], name: &str) -> io::Result<bool> {
    if !stderr.is_empty() {
        return Err(io::Error::other(
            "OCI container inspection produced diagnostics",
        ));
    }
    if stdout.is_empty() {
        return Ok(false);
    }
    if stdout != format!("{name}\n").as_bytes() {
        return Err(io::Error::other(
            "OCI container name selection is ambiguous",
        ));
    }
    Ok(true)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Ownership {
    id: String,
    image: String,
    owner: String,
    manifest: Option<Descriptor>,
}

pub(super) fn ownership(bytes: &[u8], image: &EngineImage<'_>, name: &str) -> io::Result<String> {
    let ownership: Ownership = serde_json::from_slice(bytes).map_err(io::Error::other)?;
    if ownership.id.len() != 64
        || !ownership
            .id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        || !image.container_matches(&ownership.image, ownership.manifest.as_ref())
        || ownership.owner != name
    {
        return Err(io::Error::other(
            "OCI container ownership differs; control refused",
        ));
    }
    Ok(ownership.id)
}

fn inspect<F>(
    docker: &mut Docker<'_, F>,
    runtime: &ActiveRuntime<'_>,
    name: &str,
    perform: impl FnOnce(&mut Command, Duration) -> Result<(), ProcessError>,
) -> io::Result<String>
where
    F: FnMut(&[String], &Path, &Path) -> io::Result<Command>,
{
    let format = "{\"id\":{{json .Id}},\"image\":{{json .Image}},\"owner\":{{json (index .Config.Labels \"io.rusterisk.distribution.check\")}},\"manifest\":{{with index . \"ImageManifestDescriptor\"}}{\"digest\":{{json .digest}},\"media_type\":{{json .mediaType}},\"size\":{{json .size}}}{{else}}null{{end}}}";
    let observed = docker.run_with(
        &arguments(["container", "inspect", "--format", format, name]),
        Duration::from_secs(15),
        "ownership",
        perform,
    )?;
    if !observed.stderr.is_empty() {
        return Err(io::Error::other(
            "OCI ownership inspection produced diagnostics",
        ));
    }
    ownership(&observed.stdout, &runtime.image, name)
}

fn cleanup<F, G>(
    docker: &mut Docker<'_, F>,
    supervisor: &mut RuntimeCommands<'_, G>,
    runtime: &ActiveRuntime<'_>,
    phase: ServerOciRunPhase,
) -> io::Result<()>
where
    F: FnMut(&[String], &Path, &Path) -> io::Result<Command>,
    G: FnMut(&mut Command, Duration) -> io::Result<()>,
{
    let name = runtime.plan.container(phase);
    if !present(docker, supervisor, &name, true)? {
        return Ok(());
    }
    let ownership = inspect(docker, runtime, &name, |command, budget| {
        supervisor.run_cleanup(command, budget)
    })?;
    let step = runtime.plan.cleanup(phase);
    docker.run(
        supervisor,
        &arguments(["rm", "--force", &ownership]),
        step.budget,
        "remove",
        true,
    )?;
    if present(docker, supervisor, &name, true)? {
        return Err(io::Error::other(
            "owned OCI container remains after removal",
        ));
    }
    Ok(())
}

fn serve<F, G>(
    docker: &mut Docker<'_, F>,
    supervisor: &mut RuntimeCommands<'_, G>,
    runtime: &ActiveRuntime<'_>,
    address: SocketAddr,
    client: impl FnOnce(&mut RuntimeService<'_, G>, SocketAddr) -> io::Result<()>,
) -> io::Result<()>
where
    F: FnMut(&[String], &Path, &Path) -> io::Result<Command>,
    G: FnMut(&mut Command, Duration) -> io::Result<()>,
{
    let name = runtime.plan.container(ServerOciRunPhase::Serve);
    if present(docker, supervisor, &name, false)? {
        return Err(io::Error::other("OCI runtime container already exists"));
    }
    let step = runtime.plan.step(ServerOciRunPhase::Serve);
    let mut capture = docker.command(&step.arguments, "serve")?;
    let mut service = supervisor
        .start_service(&mut capture.command, step.budget)
        .map_err(io::Error::other)?;
    let mut ownership = None;
    let mut operation = (|| {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            service.poll().map_err(io::Error::other)?;
            let observed = capture.read()?;
            if check_server_ready_log(&observed.stdout, address)? {
                break;
            }
            if Instant::now() >= deadline {
                return Err(io::Error::other("OCI server readiness deadline expired"));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        ownership = Some(inspect(docker, runtime, &name, |command, budget| {
            service.run(command, budget)
        })?);
        let operation = client(&mut service, address);
        combine(
            operation,
            service.poll().map_err(io::Error::other),
            "OCI service lifetime",
        )
    })();
    if ownership.is_none() {
        match inspect(docker, runtime, &name, |command, budget| {
            service.run(command, budget)
        }) {
            Ok(owned) => ownership = Some(owned),
            Err(error) => operation = combine(operation, Err(error), "OCI shutdown ownership"),
        }
    }
    let step = runtime.plan.stop();
    let stopped = match ownership {
        Some(owned) => match docker.command(&arguments(["stop", "--timeout=5", &owned]), "stop") {
            Ok(mut request) => {
                let result = service
                    .stop_with(&mut request.command, step.budget)
                    .map_err(io::Error::other);
                combine(
                    result,
                    request.read().map(|_| ()),
                    "OCI stop log preservation",
                )
            }
            Err(error) => combine(
                Err(error),
                service.stop().map_err(io::Error::other),
                "OCI client cleanup",
            ),
        },
        None => service.stop().map_err(io::Error::other),
    };
    let operation = combine(operation, stopped, "OCI explicit shutdown");
    let checked = (|| {
        let observed = capture.read()?;
        if !observed.stderr.is_empty() || !check_server_ready_log(&observed.stdout, address)? {
            return Err(io::Error::other("OCI runtime diagnostics changed"));
        }
        Ok(())
    })();
    combine(operation, checked, "OCI runtime diagnostics")
}

pub(super) fn runtime_using<F>(
    runtime: Runtime<'_>,
    supervisor: &mut CommandSupervisor,
    factory: F,
    client: impl FnOnce(&mut RunningCommand<'_>, SocketAddr) -> io::Result<()>,
) -> io::Result<()>
where
    F: FnMut(&[String], &Path, &Path) -> io::Result<Command>,
{
    let mut configure = |_: &mut Command, _: Duration| Ok(());
    let mut commands = RuntimeCommands::new(supervisor, &mut configure);
    runtime_configured_using(runtime, &mut commands, factory, |service, address| {
        client(service.running(), address)
    })
    .map(|_| ())
}

pub(super) fn runtime_configured_using<F, G>(
    runtime: Runtime<'_>,
    supervisor: &mut RuntimeCommands<'_, G>,
    factory: F,
    client: impl FnOnce(&mut RuntimeService<'_, G>, SocketAddr) -> io::Result<()>,
) -> io::Result<RuntimeObservation>
where
    F: FnMut(&[String], &Path, &Path) -> io::Result<Command>,
    G: FnMut(&mut Command, Duration) -> io::Result<()>,
{
    let mut docker = Docker::new(factory, runtime.artifacts);
    let scope = ServerOciRuntimePlan::new(
        runtime.identity,
        runtime.expected.configuration,
        runtime.configuration,
    )?;
    for phase in ServerOciRunPhase::ALL {
        if present(&mut docker, supervisor, &scope.container(phase), false)? {
            return Err(io::Error::other("OCI runtime container already exists"));
        }
    }
    let archive = runtime
        .archive
        .to_str()
        .ok_or_else(|| io::Error::other("OCI archive path is not UTF-8"))?;
    let loaded = docker.run(
        supervisor,
        &arguments(["load", "--input", archive]),
        Duration::from_secs(60),
        "load",
        false,
    )?;
    let image = EngineImage::loaded(&loaded.stdout, runtime.expected)?;
    let observed = docker.run(
        supervisor,
        &arguments(["image", "inspect", "--format", INSPECT, image.id()]),
        Duration::from_secs(15),
        "image",
        false,
    )?;
    if !observed.stderr.is_empty() {
        return Err(io::Error::other(
            "OCI image inspection produced diagnostics",
        ));
    }
    image.check(&observed.stdout)?;
    write(
        &runtime.artifacts.join("IMAGE.id"),
        format!("{}\n", image.id()).as_bytes(),
    )?;
    let active = ActiveRuntime {
        plan: ServerOciRuntimePlan::new(runtime.identity, image.id(), runtime.configuration)?,
        image,
        version: runtime.version,
        http_reservation: runtime.http_reservation,
        sip_reservation: runtime.sip_reservation,
    };
    run_loaded(active, supervisor, &mut docker, client)
}

fn run_loaded<F, G>(
    mut runtime: ActiveRuntime<'_>,
    supervisor: &mut RuntimeCommands<'_, G>,
    docker: &mut Docker<'_, F>,
    client: impl FnOnce(&mut RuntimeService<'_, G>, SocketAddr) -> io::Result<()>,
) -> io::Result<RuntimeObservation>
where
    F: FnMut(&[String], &Path, &Path) -> io::Result<Command>,
    G: FnMut(&mut Command, Duration) -> io::Result<()>,
{
    let mut attempted = Vec::with_capacity(4);
    let operation = (|| {
        for phase in [
            ServerOciRunPhase::Version,
            ServerOciRunPhase::PackagedConfig,
            ServerOciRunPhase::RuntimeConfig,
        ] {
            attempted.push(phase);
            let step = runtime.plan.step(phase);
            let observed = docker.run(
                supervisor,
                &step.arguments,
                step.budget,
                phase.label(),
                false,
            )?;
            let expected = match phase {
                ServerOciRunPhase::Version => format!("rusterisk {}\n", runtime.version),
                ServerOciRunPhase::PackagedConfig | ServerOciRunPhase::RuntimeConfig => {
                    "Configuration valid.\n".into()
                }
                ServerOciRunPhase::Serve => {
                    return Err(io::Error::other("invalid OCI command phase"))
                }
            };
            observed.exact(expected.as_bytes())?;
        }
        attempted.push(ServerOciRunPhase::Serve);
        let http_reservation = runtime
            .http_reservation
            .take()
            .ok_or_else(|| io::Error::other("OCI HTTP reservation ended"))?;
        let sip_reservation = runtime
            .sip_reservation
            .take()
            .ok_or_else(|| io::Error::other("OCI SIP reservation ended"))?;
        let http = http_reservation.local_addr()?;
        let sip = sip_reservation.local_addr()?;
        drop(http_reservation);
        drop(sip_reservation);
        serve(docker, supervisor, &runtime, http, client)?;
        let http_reservation = TcpListener::bind(http)?;
        let sip_reservation = UdpSocket::bind(sip)?;
        drop(http_reservation);
        drop(sip_reservation);
        serve(docker, supervisor, &runtime, http, |_, _| Ok(()))?;
        let _http = TcpListener::bind(http)?;
        let _sip = UdpSocket::bind(sip)?;
        Ok(RuntimeObservation {
            http,
            sip,
            profile: runtime.image.profile(),
        })
    })();
    let mut removed = Ok(());
    for phase in attempted.into_iter().rev() {
        removed = combine(
            removed,
            cleanup(docker, supervisor, &runtime, phase),
            "OCI container cleanup",
        );
    }
    combine(operation, removed, "OCI owned resources")
}
