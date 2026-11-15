use super::{step, valid_identity, ReferenceError, Step};
use std::net::SocketAddr;

pub struct Interop {
    name: String,
    configuration: String,
}

pub fn callback_address(output: &str) -> Result<std::net::Ipv4Addr, ReferenceError> {
    if output.len() > 128 {
        return Err(ReferenceError::InvalidAddress);
    }
    let mut selected = None;
    for line in output.lines() {
        let mut fields = line.split_whitespace();
        let address = fields
            .next()
            .ok_or(ReferenceError::InvalidAddress)?
            .parse::<std::net::Ipv4Addr>()
            .map_err(|_| ReferenceError::InvalidAddress)?;
        if !matches!(fields.next(), Some("STREAM" | "DGRAM" | "RAW")) || fields.count() > 1 {
            return Err(ReferenceError::InvalidAddress);
        }
        if address.is_unspecified()
            || address.is_multicast()
            || address.is_broadcast()
            || address.is_loopback()
            || selected.is_some_and(|selected| selected != address)
        {
            return Err(ReferenceError::InvalidAddress);
        }
        selected = Some(address);
    }
    selected.ok_or(ReferenceError::InvalidAddress)
}

pub fn media_address(output: &str) -> Result<std::net::Ipv4Addr, ReferenceError> {
    if output.len() > 128 {
        return Err(ReferenceError::InvalidAddress);
    }
    let address: std::net::Ipv4Addr = output
        .trim()
        .parse()
        .map_err(|_| ReferenceError::InvalidAddress)?;
    if !address.is_private() {
        return Err(ReferenceError::InvalidAddress);
    }
    Ok(address)
}

pub fn published_address(output: &str) -> Result<SocketAddr, ReferenceError> {
    if output.len() > 128 {
        return Err(ReferenceError::InvalidAddress);
    }
    let address: SocketAddr = output
        .trim()
        .parse()
        .map_err(|_| ReferenceError::InvalidAddress)?;
    if address.ip() != std::net::Ipv4Addr::LOCALHOST || address.port() == 0 {
        return Err(ReferenceError::InvalidAddress);
    }
    Ok(address)
}

impl Interop {
    pub fn new(identity: &str, configuration: &str) -> Result<Self, ReferenceError> {
        if !valid_identity(identity) {
            return Err(ReferenceError::InvalidIdentity);
        }
        if configuration.is_empty()
            || configuration.len() > 4096
            || configuration.contains([',', '\r', '\n', '\0'])
        {
            return Err(ReferenceError::InvalidConfigPath);
        }
        Ok(Self {
            name: format!("rusterisk-interop-{identity}"),
            configuration: configuration.into(),
        })
    }

    pub fn start(&self) -> Step {
        step(
            &[
                "run",
                "--detach",
                "--name",
                &self.name,
                "--cpus=1",
                "--memory=512m",
                "--memory-swap=512m",
                "--pids-limit=128",
                "--cap-drop=ALL",
                "--security-opt=no-new-privileges",
                "--read-only",
                "--tmpfs",
                "/var/run/asterisk:uid=65532,gid=65532,mode=0700,size=8m",
                "--tmpfs",
                "/var/spool/asterisk:uid=65532,gid=65532,mode=0700,size=32m",
                "--tmpfs",
                "/var/log/asterisk:uid=65532,gid=65532,mode=0700,size=8m",
                "--tmpfs",
                "/tmp:mode=1777,size=16m",
                "--publish",
                "127.0.0.1::8088",
                "--mount",
                &format!(
                    "type=bind,source={},target=/etc/asterisk,readonly",
                    self.configuration
                ),
                "--entrypoint",
                "/usr/bin/timeout",
                "rusterisk-asterisk-reference:22.9.0",
                "600",
                "/usr/sbin/asterisk",
                "-f",
                "-g",
                "-vvv",
                "-C",
                "/etc/asterisk/asterisk.conf",
            ],
            30,
        )
    }

    pub fn port(&self) -> Step {
        step(&["port", &self.name, "8088/tcp"], 10)
    }

    pub fn media_address(&self) -> Step {
        step(
            &[
                "inspect",
                "--format",
                "{{range .NetworkSettings.Networks}}{{.IPAddress}}{{\"\\n\"}}{{end}}",
                &self.name,
            ],
            10,
        )
    }

    pub fn callback_address(&self) -> Step {
        step(
            &[
                "exec",
                &self.name,
                "getent",
                "ahostsv4",
                "host.docker.internal",
            ],
            10,
        )
    }

    pub fn logs(&self) -> Step {
        step(&["logs", "--tail", "80", &self.name], 10)
    }

    pub fn cleanup(&self) -> Step {
        step(&["rm", "--force", &self.name], 30)
    }
}
