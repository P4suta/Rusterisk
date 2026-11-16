use super::{step, Interop, ReferenceError, Step};
use std::net::{Ipv4Addr, SocketAddr};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SippAdapter {
    Asterisk,
    Rusterisk,
}

impl SippAdapter {
    fn statistics(self) -> &'static str {
        match self {
            Self::Asterisk => "/tmp/rusterisk-sipp-asterisk.csv",
            Self::Rusterisk => "/tmp/rusterisk-sipp-rusterisk.csv",
        }
    }
}

impl Interop {
    pub fn sipp_registration(
        &self,
        adapter: SippAdapter,
        target: SocketAddr,
        local: Ipv4Addr,
    ) -> Result<Step, ReferenceError> {
        let SocketAddr::V4(target_address) = target else {
            return Err(ReferenceError::InvalidAddress);
        };
        if target_address.port() == 0
            || target_address.ip().is_unspecified()
            || target_address.ip().is_multicast()
            || target_address.ip().is_broadcast()
            || !local.is_private()
        {
            return Err(ReferenceError::InvalidAddress);
        }
        Ok(step(
            &[
                "exec",
                "--workdir",
                "/tmp",
                &self.name,
                "/usr/bin/timeout",
                "--kill-after=2s",
                "20s",
                "/usr/local/bin/sipp",
                &target.to_string(),
                "-sf",
                "/etc/asterisk/sipp-registration.xml",
                "-i",
                &local.to_string(),
                "-p",
                "0",
                "-bind_local",
                "-t",
                "u1",
                "-m",
                "1",
                "-l",
                "1",
                "-r",
                "1",
                "-timeout",
                "15s",
                "-timeout_error",
                "-recv_timeout",
                "2000",
                "-send_timeout",
                "2000",
                "-nostdin",
                "-default_behaviors",
                "abortunexp",
                "-trace_stat",
                "-stat_delimiter",
                ";",
                "-stf",
                adapter.statistics(),
                "-fd",
                "1",
                "-au",
                "phone",
                "-ap",
                "test-password",
            ],
            30,
        ))
    }

    pub fn sipp_statistics(&self, adapter: SippAdapter) -> Step {
        step(&["exec", &self.name, "cat", "--", adapter.statistics()], 10)
    }
}
