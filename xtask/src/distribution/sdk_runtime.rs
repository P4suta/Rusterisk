use super::{sdk_consumer_lock, SdkConsumerLockError, SdkContents, SdkPackage};

#[derive(Debug)]
pub struct SdkRuntimeIdentity {
    pub(super) version: String,
    pub(super) source: String,
    pub(super) checksum: String,
}

impl SdkRuntimeIdentity {
    pub fn from_lock(bytes: &[u8]) -> Result<Self, SdkConsumerLockError> {
        sdk_consumer_lock::runtime(bytes)
    }

    pub fn from_contents(contents: &SdkContents) -> Result<Self, SdkConsumerLockError> {
        let lock = contents
            .archive(SdkPackage::Client)
            .files()
            .iter()
            .find(|file| file.path().as_str() == "Cargo.lock")
            .ok_or(SdkConsumerLockError::Contract("client seed lockfile"))?;
        Self::from_lock(lock.bytes())
    }

    pub fn version(&self) -> &str {
        &self.version
    }

    pub fn source(&self) -> &str {
        &self.source
    }

    pub fn checksum(&self) -> &str {
        &self.checksum
    }
}
