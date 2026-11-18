use super::retained::{CleanupOutputs, ToolOutputs};
use super::{
    commands::{arguments, combine, docker, owned_step, read, Commands},
    ImageIdentity, Interop, ReferenceImage, Step,
};
use crate::process::{CommandSupervisor, ProcessError};
use serde::Deserialize;
use std::{
    io,
    path::PathBuf,
    process::Command,
    time::{Duration, Instant},
};
const REFERENCE_IMAGE: &str = "rusterisk-asterisk-reference:22.9.0";
const OWNERSHIP_LABEL: &str = "io.rusterisk.sipp-comparison";
pub(super) fn container_id(bytes: &[u8]) -> io::Result<&str> {
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

fn check_ownership(
    ownership: Ownership,
    owner: &str,
    id: &str,
    image: &str,
    name: &str,
) -> io::Result<String> {
    if ownership.id != id
        || ownership.requested_image != image
        || ownership.name != format!("/{name}")
        || ownership.owner.as_deref() != Some(owner)
    {
        return Err(io::Error::other(
            "comparison container ownership differs; control refused",
        ));
    }
    Ok(ownership.id)
}

pub(super) fn validate_reference_ownership(
    source: &str,
    owner: &str,
    id: &str,
    image: &str,
    name: &str,
) -> io::Result<String> {
    container_id(id.as_bytes())?;
    check_ownership(
        serde_json::from_str(source).map_err(io::Error::other)?,
        owner,
        id,
        image,
        name,
    )
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

pub(super) struct Reference {
    name: String,
    owner: String,
    cidfile: PathBuf,
    image: Option<String>,
    attempted: bool,
}

impl Reference {
    pub(super) fn new(owner: &str, cidfile: PathBuf, plan: &Interop) -> io::Result<Self> {
        let start = plan.start();
        let name = start
            .arguments
            .windows(2)
            .find(|parts| parts[0] == "--name")
            .map(|parts| parts[1].clone())
            .ok_or_else(|| io::Error::other("comparison container name missing"))?;
        Ok(Self {
            name,
            owner: owner.into(),
            cidfile,
            image: None,
            attempted: false,
        })
    }
    pub(super) fn name(&self) -> &str {
        &self.name
    }

    pub(super) fn launch(
        &mut self,
        image: &ReferenceImage,
        mut start: Step,
        commands: &mut Commands<'_>,
        supervisor: &mut CommandSupervisor,
    ) -> io::Result<String> {
        self.image = Some(image.as_str().into());
        let cidfile = self
            .cidfile
            .to_str()
            .ok_or_else(|| io::Error::other("comparison CID path is not UTF-8"))?;
        for value in [
            cidfile.to_owned(),
            "--cidfile".into(),
            format!("{OWNERSHIP_LABEL}={}", self.owner),
            "--label".into(),
        ] {
            start.arguments.insert(1, value);
        }
        self.attempted = true;
        commands.run(docker(&start.arguments), start.budget, |command, budget| {
            supervisor.run(command, budget)
        })?;
        let bytes = read(&self.cidfile, 65)?;
        self.inspect(container_id(&bytes)?, commands, |command, budget| {
            supervisor.run(command, budget)
        })
    }

    fn check(&self, id: &str, ownership: Ownership) -> io::Result<String> {
        let image = self
            .image
            .as_deref()
            .ok_or_else(|| io::Error::other("comparison image is missing"))?;
        check_ownership(ownership, &self.owner, id, image, &self.name)
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

    pub(super) fn cleanup_recorded(
        &self,
        commands: &mut Commands<'_>,
        supervisor: &mut CommandSupervisor,
    ) -> io::Result<Option<CleanupOutputs>> {
        let located = self.locate(commands, supervisor)?;
        let operation = (|| {
            let Some(id) = located.id else {
                return Ok(None);
            };
            let id = self.inspect(&id, commands, |command, budget| {
                supervisor.run_cleanup(command, budget)
            })?;
            let inspection = commands.last_output();
            commands.run(
                docker(&arguments(["rm", "--force", &id])),
                Duration::from_secs(30),
                |command, budget| supervisor.run_cleanup(command, budget),
            )?;
            let removal = commands.last_output();
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
                .exact(b"")?;
            Ok(Some(CleanupOutputs {
                inspection,
                removal,
                absence: commands.last_output(),
            }))
        })();
        combine(
            operation,
            located.damage.map_or(Ok(()), Err),
            "comparison CID recovery",
        )
    }

    pub(super) fn diagnostics(
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

pub(super) fn start_reference(
    reference: &mut Reference,
    plan: &Interop,
    selected: Option<&ReferenceImage>,
    commands: &mut Commands<'_>,
    supervisor: &mut CommandSupervisor,
) -> io::Result<String> {
    let inspected = commands.run(
        docker(&arguments([
            "image",
            "inspect",
            "--format",
            "{{.Id}}",
            selected.map_or(REFERENCE_IMAGE, ReferenceImage::as_str),
        ])),
        Duration::from_secs(10),
        |command, budget| supervisor.run(command, budget),
    )?;
    let source = inspected.text()?;
    let source = source.strip_suffix('\n').unwrap_or(source);
    let image = ImageIdentity::parse(source)
        .filter(|image| image.as_str().starts_with("sha256:"))
        .ok_or_else(|| io::Error::other("comparison reference image ID is invalid"))?;
    if selected.is_some_and(|selected| selected.as_str() != image.as_str()) {
        return Err(io::Error::other(
            "comparison reference image differs from the selected image",
        ));
    }
    let image = ReferenceImage::new(image);
    let start = image.start(plan).map_err(io::Error::other)?;
    reference.launch(&image, start, commands, supervisor)
}

pub(super) fn reference_versions(
    id: &str,
    commands: &mut Commands<'_>,
    supervisor: &mut CommandSupervisor,
) -> io::Result<ToolOutputs> {
    commands
        .run(
            docker(&arguments(["exec", id, "/usr/sbin/asterisk", "-V"])),
            Duration::from_secs(10),
            |command, budget| supervisor.run(command, budget),
        )?
        .exact(b"Asterisk 22.9.0\n")?;
    let asterisk = commands.last_output();
    let mut status = None;
    let observed = commands.run(
        docker(&arguments(["exec", id, "/usr/local/bin/sipp", "-v"])),
        Duration::from_secs(10),
        |command, budget| match supervisor.run(command, budget) {
            Err(ProcessError::Status(observed)) if observed.code() == Some(99) => {
                status = observed.code();
                Ok(())
            }
            Ok(()) => Err(ProcessError::InvalidOutput),
            Err(error) => Err(error),
        },
    )?;
    let status = status.ok_or_else(|| io::Error::other("comparison SIPp status is missing"))?;
    validate_sipp_version(status, observed.text()?)?;
    Ok(ToolOutputs {
        asterisk,
        sipp: commands.last_output(),
        sipp_status: status,
    })
}

pub(super) fn validate_sipp_version(status: i32, text: &str) -> io::Result<()> {
    if status != 99
        || text
            .lines()
            .filter(|line| *line == " SIPp v3.8.0-TLS.")
            .count()
            != 1
    {
        return Err(io::Error::other(
            "comparison SIPp version or exit status differs",
        ));
    }
    Ok(())
}

pub(super) fn reference_ready(
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
