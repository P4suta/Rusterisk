use super::{
    commands::{arguments, combine, docker, Commands},
    resources::container_id,
    valid_identity,
};
use crate::process::{CommandSupervisor, ProcessError};
use serde::Deserialize;
use std::{io, process::Command, time::Duration};

const OWNERSHIP_LABEL: &str = "io.rusterisk.browser-comparison";
const GATEWAY_OPTION: &str = "com.docker.network.bridge.gateway_mode_ipv4";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Ownership {
    id: String,
    name: String,
    owner: Option<String>,
    driver: String,
    internal: bool,
    gateway_mode_ipv4: Option<String>,
}

fn check_ownership(source: &str, owner: &str, id: &str, name: &str) -> io::Result<String> {
    container_id(id.as_bytes())?;
    let observed: Ownership = serde_json::from_str(source).map_err(io::Error::other)?;
    if observed.id != id
        || observed.name != name
        || observed.owner.as_deref() != Some(owner)
        || observed.driver != "bridge"
        || !observed.internal
        || observed.gateway_mode_ipv4.as_deref() != Some("isolated")
    {
        return Err(io::Error::other(
            "browser network ownership or isolation differs; control refused",
        ));
    }
    Ok(observed.id)
}

fn unique_id(bytes: &[u8]) -> io::Result<Option<String>> {
    if bytes.is_empty() {
        Ok(None)
    } else {
        container_id(bytes).map(|id| Some(id.to_owned()))
    }
}

fn locate(
    filter: &str,
    commands: &mut Commands<'_>,
    perform: impl FnOnce(&mut Command, Duration) -> Result<(), ProcessError>,
) -> io::Result<Option<String>> {
    let observed = commands.run(
        docker(&arguments([
            "network",
            "ls",
            "--no-trunc",
            "--filter",
            filter,
            "--format",
            "{{.ID}}",
        ])),
        Duration::from_secs(10),
        perform,
    )?;
    observed.text()?;
    unique_id(&observed.stdout)
}

enum State {
    Fresh,
    Attempted,
    Created(String),
    Cleaned,
}

pub(super) struct BrowserNetwork {
    name: String,
    owner: String,
    state: State,
}

impl BrowserNetwork {
    pub(super) fn new(identity: &str) -> io::Result<Self> {
        if !valid_identity(identity) {
            return Err(io::Error::other("browser network identity is invalid"));
        }
        Ok(Self {
            name: format!("rusterisk-browser-reference-{identity}"),
            owner: identity.to_owned(),
            state: State::Fresh,
        })
    }

    fn label_filter(&self) -> String {
        format!("label={OWNERSHIP_LABEL}={}", self.owner)
    }

    fn inspect(
        &self,
        id: &str,
        commands: &mut Commands<'_>,
        perform: impl FnOnce(&mut Command, Duration) -> Result<(), ProcessError>,
    ) -> io::Result<()> {
        let format = format!(
            "{{\"id\":{{{{json .Id}}}},\"name\":{{{{json .Name}}}},\"owner\":{{{{json (index .Labels \"{OWNERSHIP_LABEL}\")}}}},\"driver\":{{{{json .Driver}}}},\"internal\":{{{{json .Internal}}}},\"gateway_mode_ipv4\":{{{{json (index .Options \"{GATEWAY_OPTION}\")}}}}}}"
        );
        let observed = commands.run(
            docker(&arguments(["network", "inspect", "--format", &format, id])),
            Duration::from_secs(10),
            perform,
        )?;
        check_ownership(observed.text()?, &self.owner, id, &self.name).map(drop)
    }

    pub(super) fn create(
        &mut self,
        commands: &mut Commands<'_>,
        supervisor: &mut CommandSupervisor,
    ) -> io::Result<String> {
        if !matches!(self.state, State::Fresh) {
            return Err(io::Error::other(
                "browser network creation was already attempted",
            ));
        }
        for filter in [format!("name={}", self.name), self.label_filter()] {
            if locate(&filter, commands, |command, budget| {
                supervisor.run(command, budget)
            })?
            .is_some()
            {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "browser network name or run label already exists; creation refused",
                ));
            }
        }
        let label = format!("{OWNERSHIP_LABEL}={}", self.owner);
        let option = format!("{GATEWAY_OPTION}=isolated");
        self.state = State::Attempted;
        let observed = commands.run(
            docker(&arguments([
                "network",
                "create",
                "--internal",
                "--driver",
                "bridge",
                "--opt",
                &option,
                "--label",
                &label,
                &self.name,
            ])),
            Duration::from_secs(30),
            |command, budget| supervisor.run(command, budget),
        )?;
        observed.text()?;
        let id = container_id(&observed.stdout)?.to_owned();
        self.state = State::Created(id.clone());
        self.verify(&id, commands, supervisor)?;
        Ok(id)
    }

    pub(super) fn verify(
        &self,
        id: &str,
        commands: &mut Commands<'_>,
        supervisor: &mut CommandSupervisor,
    ) -> io::Result<()> {
        if !matches!(&self.state, State::Created(expected) if expected == id) {
            return Err(io::Error::other(
                "browser network ID differs from its creation; control refused",
            ));
        }
        self.inspect(id, commands, |command, budget| {
            supervisor.run(command, budget)
        })
    }

    fn absent(
        &self,
        id: Option<&str>,
        commands: &mut Commands<'_>,
        supervisor: &mut CommandSupervisor,
    ) -> io::Result<()> {
        if let Some(id) = id {
            if locate(&format!("id={id}"), commands, |command, budget| {
                supervisor.run_cleanup(command, budget)
            })?
            .is_some()
            {
                return Err(io::Error::other("browser network ID remains after cleanup"));
            }
        }
        for filter in [self.label_filter(), format!("name={}", self.name)] {
            if locate(&filter, commands, |command, budget| {
                supervisor.run_cleanup(command, budget)
            })?
            .is_some()
            {
                return Err(io::Error::other(
                    "browser network name or run label remains; absence is unproven",
                ));
            }
        }
        Ok(())
    }

    pub(super) fn cleanup(
        &mut self,
        commands: &mut Commands<'_>,
        supervisor: &mut CommandSupervisor,
    ) -> io::Result<()> {
        if matches!(self.state, State::Fresh | State::Cleaned) {
            self.state = State::Cleaned;
            return Ok(());
        }
        let selected = locate(&self.label_filter(), commands, |command, budget| {
            supervisor.run_cleanup(command, budget)
        })?;
        let Some(id) = selected else {
            let expected = match &self.state {
                State::Created(id) => Some(id.as_str()),
                State::Attempted => None,
                State::Fresh | State::Cleaned => {
                    return Err(io::Error::other("browser network cleanup state changed"));
                }
            };
            self.absent(expected, commands, supervisor)?;
            self.state = State::Cleaned;
            return Ok(());
        };
        if matches!(&self.state, State::Created(expected) if expected != &id) {
            return Err(io::Error::other(
                "browser network run label selected another ID; removal refused",
            ));
        }
        self.inspect(&id, commands, |command, budget| {
            supervisor.run_cleanup(command, budget)
        })?;
        let removal = commands
            .run(
                docker(&arguments(["network", "rm", &id])),
                Duration::from_secs(30),
                |command, budget| supervisor.run_cleanup(command, budget),
            )
            .and_then(|observed| observed.text().map(drop));
        combine(
            removal,
            self.absent(Some(&id), commands, supervisor),
            "browser network absence verification failed",
        )?;
        self.state = State::Cleaned;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{check_ownership, unique_id};
    use std::io;

    const OWNER: &str = "fixture";
    const NAME: &str = "rusterisk-browser-reference-fixture";

    fn ownership(id: &str) -> serde_json::Value {
        serde_json::json!({
            "id": id,
            "name": NAME,
            "owner": OWNER,
            "driver": "bridge",
            "internal": true,
            "gateway_mode_ipv4": "isolated"
        })
    }

    #[test]
    fn network_ownership_accepts_only_the_exact_run_and_isolated_bridge() -> io::Result<()> {
        let id = "a".repeat(64);
        let expected = ownership(&id);
        assert_eq!(
            check_ownership(&expected.to_string(), OWNER, &id, NAME)?,
            id
        );
        for (field, value) in [
            ("id", serde_json::json!("b".repeat(64))),
            ("name", serde_json::json!("foreign-network")),
            ("owner", serde_json::json!("foreign-run")),
            ("owner", serde_json::Value::Null),
            ("driver", serde_json::json!("host")),
            ("internal", serde_json::json!(false)),
            ("gateway_mode_ipv4", serde_json::json!("nat")),
            ("gateway_mode_ipv4", serde_json::Value::Null),
        ] {
            let mut changed = expected.clone();
            changed[field] = value;
            assert!(check_ownership(&changed.to_string(), OWNER, &id, NAME).is_err());
        }
        Ok(())
    }

    #[test]
    fn network_ownership_rejects_malformed_or_incomplete_inspection() -> io::Result<()> {
        let id = "a".repeat(64);
        for field in [
            "id",
            "name",
            "owner",
            "driver",
            "internal",
            "gateway_mode_ipv4",
        ] {
            let mut changed = ownership(&id);
            changed
                .as_object_mut()
                .ok_or_else(|| io::Error::other("network fixture is not an object"))?
                .remove(field)
                .ok_or_else(|| io::Error::other("network fixture field missing"))?;
            assert!(check_ownership(&changed.to_string(), OWNER, &id, NAME).is_err());
        }
        for source in ["", "[]", "null", "{}", "{\"id\":true}"] {
            assert!(check_ownership(source, OWNER, &id, NAME).is_err());
        }
        let mut changed = ownership(&id);
        changed["internal"] = serde_json::json!("true");
        assert!(check_ownership(&changed.to_string(), OWNER, &id, NAME).is_err());
        let mut changed = ownership(&id);
        changed["unexpected"] = serde_json::json!(true);
        assert!(check_ownership(&changed.to_string(), OWNER, &id, NAME).is_err());
        for invalid in ["a".repeat(63), "A".repeat(64), "g".repeat(64)] {
            let changed = ownership(&invalid);
            assert!(check_ownership(&changed.to_string(), OWNER, &invalid, NAME).is_err());
        }
        Ok(())
    }

    #[test]
    fn recovery_requires_one_complete_network_id_or_proven_absence() -> io::Result<()> {
        let id = "a".repeat(64);
        assert_eq!(unique_id(b"")?, None);
        for bytes in [id.as_bytes().to_vec(), format!("{id}\n").into_bytes()] {
            assert_eq!(unique_id(&bytes)?.as_deref(), Some(id.as_str()));
        }
        for bytes in [
            format!("{id}\n{}\n", "b".repeat(64)).into_bytes(),
            format!("{id}\n{id}\n").into_bytes(),
            format!("{id}\n\n").into_bytes(),
            format!("{id}\r\n").into_bytes(),
            b"foreign-network-name\n".to_vec(),
            vec![b'a'; 63],
            vec![b'A'; 64],
        ] {
            assert!(unique_id(&bytes).is_err());
        }
        Ok(())
    }
}
