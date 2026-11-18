use super::{step, valid_identity, ReferenceError, Step};
use std::net::SocketAddr;
mod sipp;
pub use sipp::SippAdapter;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SipTransferCase {
    Blind,
    Attended,
}

impl SipTransferCase {
    pub const ALL: [Self; 2] = [Self::Blind, Self::Attended];

    pub const fn name(self) -> &'static str {
        match self {
            Self::Blind => "blind_transfer::reference_blind_transfer_matches_native_received_audio",
            Self::Attended => {
                "attended_transfer::reference_attended_transfer_matches_native_received_audio"
            }
        }
    }

    pub fn validate_execution(self, output: &str) -> Result<(), ReferenceError> {
        super::comparison::validate_case(self.name(), output)
            .map_err(|_| ReferenceError::IncompleteTransfer)
    }
}

pub struct Interop {
    name: String,
    configuration: String,
    internal_network: Option<String>,
}

#[derive(Clone, Copy)]
pub enum CallbackHost {
    DockerGateway,
    ManagedDns,
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
            internal_network: None,
        })
    }

    #[cfg(unix)]
    pub(super) fn for_browser(
        identity: &str,
        configuration: &str,
        network_id: &str,
    ) -> Result<Self, ReferenceError> {
        let mut plan = Self::new(identity, configuration)?;
        let id = super::resources::container_id(network_id.as_bytes())
            .map_err(|_| ReferenceError::InvalidIdentity)?;
        plan.internal_network = Some(id.into());
        Ok(plan)
    }

    pub fn start(&self) -> Step {
        self.start_for(if cfg!(target_os = "linux") {
            CallbackHost::DockerGateway
        } else {
            CallbackHost::ManagedDns
        })
    }

    pub fn start_for(&self, callback: CallbackHost) -> Step {
        let [network_flag, network_value] = self
            .internal_network
            .as_deref()
            .map_or(["--publish", "127.0.0.1::8088"], |id| ["--network", id]);
        let mut start = step(
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
                "/var/spool/asterisk/recording:uid=65532,gid=65532,mode=0700,size=16m",
                "--tmpfs",
                "/var/log/asterisk:uid=65532,gid=65532,mode=0700,size=8m",
                "--tmpfs",
                "/tmp:mode=1777,size=16m",
                network_flag,
                network_value,
                "--mount",
                &format!(
                    "type=bind,source={},target=/etc/asterisk,readonly",
                    self.configuration
                ),
                "--mount",
                &format!(
                    "type=bind,source={}/sounds,target=/var/lib/asterisk/sounds,readonly",
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
        );
        match (self.internal_network.as_ref(), callback) {
            (Some(_), _) => start.arguments.insert(1, "--pull=never".into()),
            (None, CallbackHost::DockerGateway) => start
                .arguments
                .insert(1, "--add-host=host.docker.internal:host-gateway".into()),
            (None, CallbackHost::ManagedDns) => {}
        }
        start
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

#[cfg(all(test, unix))]
mod browser_tests {
    use super::*;

    #[test]
    fn browser_reference_uses_only_its_owned_internal_network() -> Result<(), ReferenceError> {
        let network = "a".repeat(64);
        let plan = Interop::for_browser("browser", "/fixture/asterisk", &network)?;
        let arguments = plan.start().arguments;
        assert!(arguments
            .windows(2)
            .any(|pair| pair == ["--network", network.as_str()]));
        assert!(arguments.iter().any(|argument| argument == "--pull=never"));
        assert!(!arguments
            .iter()
            .any(|argument| { argument == "--publish" || argument.starts_with("--add-host") }));
        assert!(Interop::for_browser("browser", "/fixture/asterisk", "bridge").is_err());
        Ok(())
    }
}
