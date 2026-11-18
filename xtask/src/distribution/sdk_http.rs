use crate::process::{CommandRequestError, SupervisedCommand};
use std::net::SocketAddr;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SdkHttpTargetError {
    Address,
    Credentials,
    Version,
    Bridge,
}

impl std::fmt::Display for SdkHttpTargetError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Address => "SDK HTTP target requires a bound loopback address",
            Self::Credentials => "SDK HTTP fixture credentials are invalid",
            Self::Version => "SDK HTTP target requires a semantic version",
            Self::Bridge => "SDK HTTP target requires a reserved portable bridge identifier",
        })
    }
}
impl std::error::Error for SdkHttpTargetError {}

pub struct SdkHttpTarget<'a> {
    address: SocketAddr,
    username: &'a str,
    password: &'a str,
    version: &'a str,
    bridge: &'a str,
}

impl<'a> SdkHttpTarget<'a> {
    pub fn new(
        address: SocketAddr,
        username: &'a str,
        password: &'a str,
        version: &'a str,
        bridge: &'a str,
    ) -> Result<Self, SdkHttpTargetError> {
        if !address.ip().is_loopback() || address.port() == 0 {
            return Err(SdkHttpTargetError::Address);
        }
        if username.is_empty()
            || username.len() > 128
            || username.contains(':')
            || username.chars().any(char::is_control)
            || password.is_empty()
            || password.len() > 1024
            || password.chars().any(char::is_control)
        {
            return Err(SdkHttpTargetError::Credentials);
        }
        if version.len() > 128 || semver::Version::parse(version).is_err() {
            return Err(SdkHttpTargetError::Version);
        }
        if bridge.is_empty()
            || bridge.len() > 128
            || matches!(bridge, "." | "..")
            || !bridge
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        {
            return Err(SdkHttpTargetError::Bridge);
        }
        Ok(Self {
            address,
            username,
            password,
            version,
            bridge,
        })
    }

    pub const fn address(&self) -> SocketAddr {
        self.address
    }

    pub fn configure(&self, command: &mut SupervisedCommand) -> Result<(), CommandRequestError> {
        command
            .env(
                "RUSTERISK_SDK_HTTP_ADDRESS",
                format!("http://{}", self.address),
            )?
            .env("RUSTERISK_SDK_HTTP_USERNAME", self.username)?
            .env("RUSTERISK_SDK_HTTP_PASSWORD", self.password)?
            .env("RUSTERISK_SDK_HTTP_VERSION", self.version)?
            .env("RUSTERISK_SDK_HTTP_BRIDGE", self.bridge)?;
        Ok(())
    }
}
