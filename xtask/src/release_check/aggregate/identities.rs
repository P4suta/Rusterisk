use super::CandidateCheckError;
use crate::{
    distribution::SdkPackage,
    release_check::{
        InputKey, LinuxOciTarget, OciOutcome, RequiredGate, SdkOutcome, ServerOutcome,
    },
    server_distribution::ServerTarget,
};

#[derive(Clone, Copy, PartialEq, Eq)]
struct ArchiveSha256([u8; 32]);

impl ArchiveSha256 {
    fn parse(value: &str, gate: RequiredGate) -> Result<Self, CandidateCheckError> {
        if value.len() != 64 {
            return Err(CandidateCheckError::Identity(gate, "archive SHA256 length"));
        }
        let mut bytes = [0; 32];
        let (pairs, _) = value.as_bytes().as_chunks::<2>();
        for (destination, pair) in bytes.iter_mut().zip(pairs) {
            let hex = std::str::from_utf8(pair)
                .map_err(|_| CandidateCheckError::Identity(gate, "archive SHA256 encoding"))?;
            *destination = u8::from_str_radix(hex, 16)
                .map_err(|_| CandidateCheckError::Identity(gate, "archive SHA256 digits"))?;
        }
        Ok(Self(bytes))
    }
}

pub(super) struct SdkArchiveIdentity<'a> {
    version: &'a str,
    projection: InputKey,
    checksums: [ArchiveSha256; 3],
}

fn sdk_checksums<'a>(
    gate: RequiredGate,
    mut checksum: impl FnMut(SdkPackage) -> &'a str,
) -> Result<[ArchiveSha256; 3], CandidateCheckError> {
    Ok([
        ArchiveSha256::parse(checksum(SdkPackage::Core), gate)?,
        ArchiveSha256::parse(checksum(SdkPackage::Api), gate)?,
        ArchiveSha256::parse(checksum(SdkPackage::Client), gate)?,
    ])
}

impl<'a> SdkArchiveIdentity<'a> {
    pub(super) fn from_sdk(outcome: &'a SdkOutcome<'_>) -> Result<Self, CandidateCheckError> {
        Ok(Self {
            version: outcome.version(),
            projection: outcome.projection(),
            checksums: sdk_checksums(RequiredGate::Sdk, |package| outcome.checksum(package))?,
        })
    }

    pub(super) fn from_server(outcome: &'a ServerOutcome<'_>) -> Result<Self, CandidateCheckError> {
        Ok(Self {
            version: outcome.version(),
            projection: outcome.sdk_projection(),
            checksums: sdk_checksums(RequiredGate::Server(outcome.target()), |package| {
                outcome.sdk_checksum(package)
            })?,
        })
    }

    pub(super) fn from_oci(outcome: &'a OciOutcome<'_>) -> Result<Self, CandidateCheckError> {
        Ok(Self {
            version: outcome.version(),
            projection: outcome.sdk_projection(),
            checksums: sdk_checksums(RequiredGate::Oci(outcome.target()), |package| {
                outcome.sdk_checksum(package)
            })?,
        })
    }

    #[cfg(all(test, unix))]
    pub(super) fn from_candidate(
        source: &crate::distribution::CapturedSdk,
        contents: &'a crate::distribution::SdkContents,
    ) -> Result<Self, CandidateCheckError> {
        let projection = crate::release_check::SourceInputs::new(source.files())
            .map_err(|error| CandidateCheckError::Inputs {
                gate: RequiredGate::Sdk,
                error,
            })?
            .key();
        Ok(Self {
            version: contents.version(),
            projection,
            checksums: sdk_checksums(RequiredGate::Sdk, |package| contents.checksum(package))?,
        })
    }
}

pub(super) fn require_same_sdk(
    gate: RequiredGate,
    authoritative: &SdkArchiveIdentity<'_>,
    consumer: &SdkArchiveIdentity<'_>,
) -> Result<(), CandidateCheckError> {
    if authoritative.version != consumer.version {
        return Err(CandidateCheckError::Identity(gate, "SDK version differs"));
    }
    if authoritative.projection != consumer.projection {
        return Err(CandidateCheckError::Identity(
            gate,
            "SDK input projection differs",
        ));
    }
    if authoritative.checksums != consumer.checksums {
        return Err(CandidateCheckError::Identity(
            gate,
            "SDK archive SHA256 differs",
        ));
    }
    Ok(())
}

pub(super) struct ServerArchiveIdentity<'a> {
    target: ServerTarget,
    version: &'a str,
    projection: InputKey,
    source_digest: &'a str,
    checksum: ArchiveSha256,
}

impl<'a> ServerArchiveIdentity<'a> {
    pub(super) fn from_server(outcome: &'a ServerOutcome<'_>) -> Result<Self, CandidateCheckError> {
        Ok(Self {
            target: outcome.target(),
            version: outcome.version(),
            projection: outcome.projection(),
            source_digest: outcome.source_digest(),
            checksum: ArchiveSha256::parse(
                outcome.checksum(),
                RequiredGate::Server(outcome.target()),
            )?,
        })
    }

    pub(super) fn from_oci(outcome: &'a OciOutcome<'_>) -> Result<Self, CandidateCheckError> {
        Ok(Self {
            target: outcome.target().server_target(),
            version: outcome.version(),
            projection: outcome.projection(),
            source_digest: outcome.source_digest(),
            checksum: ArchiveSha256::parse(
                outcome.server_checksum(),
                RequiredGate::Oci(outcome.target()),
            )?,
        })
    }

    #[cfg(all(test, unix))]
    pub(super) fn from_candidate(
        source: &crate::server_distribution::CapturedServer,
        candidate: &'a crate::server_distribution::ServerCandidate,
    ) -> Result<Self, CandidateCheckError> {
        let gate = RequiredGate::Server(candidate.name().target());
        let projection = crate::release_check::SourceInputs::new(source.files())
            .map_err(|error| CandidateCheckError::Inputs { gate, error })?
            .key();
        Ok(Self {
            target: candidate.name().target(),
            version: candidate.name().version(),
            projection,
            source_digest: candidate.source_digest(),
            checksum: ArchiveSha256::parse(candidate.checksum(), gate)?,
        })
    }
}

pub(super) fn require_same_server(
    target: LinuxOciTarget,
    linux_server: &ServerArchiveIdentity<'_>,
    oci_server: &ServerArchiveIdentity<'_>,
) -> Result<(), CandidateCheckError> {
    let gate = RequiredGate::Oci(target);
    if linux_server.target != target.server_target() || oci_server.target != target.server_target()
    {
        return Err(CandidateCheckError::Identity(
            gate,
            "Linux server target differs",
        ));
    }
    if linux_server.version != oci_server.version {
        return Err(CandidateCheckError::Identity(
            gate,
            "Linux server version differs",
        ));
    }
    if linux_server.projection != oci_server.projection
        || linux_server.source_digest != oci_server.source_digest
    {
        return Err(CandidateCheckError::Identity(
            gate,
            "Linux server source differs",
        ));
    }
    if linux_server.checksum != oci_server.checksum {
        return Err(CandidateCheckError::Identity(
            gate,
            "Linux server archive SHA256 differs",
        ));
    }
    Ok(())
}
