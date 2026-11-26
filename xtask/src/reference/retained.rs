use super::{commands::read, ReferenceCase, ReferenceImage};
use serde::{Deserialize, Serialize};
use std::{
    io,
    net::{Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
};

mod configuration;
mod files;
pub(super) use configuration::CaseRuntime;

#[cfg(test)]
mod configuration_tests;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ToolOutputs {
    pub(super) asterisk: u8,
    pub(super) sipp: u8,
    pub(super) sipp_status: i32,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CleanupOutputs {
    pub(super) inspection: u8,
    pub(super) removal: u8,
    pub(super) absence: u8,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CaseRun {
    pub(super) owner: String,
    pub(super) configuration: PathBuf,
    pub(super) id: String,
    pub(super) address: SocketAddr,
    pub(super) media: Ipv4Addr,
    pub(super) callback: Ipv4Addr,
    pub(super) address_output: u8,
    pub(super) media_output: u8,
    pub(super) callback_output: u8,
    pub(super) tools: ToolOutputs,
    pub(super) cases: [u8; ReferenceCase::ALL.len()],
    pub(super) runtime: Option<CaseRuntime>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CaseContext {
    pub(super) run: CaseRun,
    pub(super) cleanup: CleanupOutputs,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SippRun {
    pub(super) owner: String,
    pub(super) directory: PathBuf,
    pub(super) id: String,
    pub(super) local: Ipv4Addr,
    pub(super) callback: Ipv4Addr,
    pub(super) local_output: u8,
    pub(super) callback_output: u8,
    pub(super) http: SocketAddr,
    pub(super) sip: SocketAddr,
    pub(super) tools: ToolOutputs,
    pub(super) statistics: [u8; 2],
    pub(super) version: u8,
    pub(super) configuration: u8,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SippContext {
    pub(super) run: SippRun,
    pub(super) cleanup: CleanupOutputs,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReferenceNativeContext {
    pub(super) cases: CaseContext,
    pub(super) sipp: SippContext,
}

fn invalid(reason: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, reason)
}

pub(super) fn preserve_initial(configuration: &Path, state: &Path) -> io::Result<()> {
    files::copy_initial(configuration, &state.join("initial-configuration"))
}

fn output(root: &Path, index: u8) -> io::Result<super::commands::Observed> {
    if !(1..=144).contains(&index) {
        return Err(invalid("reference native output index exceeds its profile"));
    }
    Ok(super::commands::Observed {
        stdout: read(&root.join(format!("{index:03}.stdout")), 1024 * 1024)?,
        stderr: read(&root.join(format!("{index:03}.stderr")), 1024 * 1024)?,
    })
}

fn tools(root: &Path, tools: &ToolOutputs) -> io::Result<()> {
    output(root, tools.asterisk)?.exact(b"Asterisk 22.9.0\n")?;
    let observed = output(root, tools.sipp)?;
    super::resources::validate_sipp_version(tools.sipp_status, observed.text()?)
}

fn cleanup(
    root: &Path,
    outputs: &CleanupOutputs,
    owner: &str,
    id: &str,
    image: &ReferenceImage,
    name: &str,
) -> io::Result<()> {
    let observed = output(root, outputs.inspection)?;
    super::resources::validate_reference_ownership(
        observed.text()?,
        owner,
        id,
        image.as_str(),
        name,
    )?;
    output(root, outputs.removal)?.exact(format!("{id}\n").as_bytes())?;
    output(root, outputs.absence)?.exact(b"")
}

impl ReferenceNativeContext {
    pub(crate) fn preserve(&self, state: &Path, destination: &Path) -> io::Result<()> {
        files::copy(state, destination)?;
        files::copy_case(
            &self.cases.run.configuration,
            &destination.join("case-configuration"),
        )
    }

    pub(crate) fn validate(
        &self,
        root: &Path,
        original_source: &Path,
        image: &ReferenceImage,
        version: &str,
        captured_source: &Path,
    ) -> io::Result<()> {
        let cases = &self.cases.run;
        let case_order = [
            cases.tools.asterisk,
            cases.tools.sipp,
            cases.address_output,
            cases.media_output,
            cases.callback_output,
            cases.cases[0],
            cases.cases[1],
            cases.cases[2],
            self.cases.cleanup.inspection,
            self.cases.cleanup.removal,
            self.cases.cleanup.absence,
        ];
        if !case_order.windows(2).all(|pair| pair[0] < pair[1]) {
            return Err(invalid(
                "reference native case outputs are repeated or out of order",
            ));
        }
        let plan = super::Interop::new(
            &cases.owner,
            cases
                .configuration
                .to_str()
                .ok_or_else(|| invalid("reference configuration path is not UTF-8"))?,
        )
        .map_err(io::Error::other)?;
        if cases.configuration
            != super::ReferenceRunner::configuration_path(original_source, &cases.owner)?
        {
            return Err(invalid(
                "reference configuration differs from its original source",
            ));
        }
        let case_root = root.join("reference-comparisons");
        if super::resources::container_id(&read(&case_root.join("reference.cid"), 65)?)? != cases.id
        {
            return Err(invalid(
                "reference container ID differs from its native file",
            ));
        }
        let inspected = output(&root.join("reference-image"), 1)?;
        if ReferenceImage::parse_inspection(inspected.text()?)
            .map_err(io::Error::other)?
            .as_str()
            != image.as_str()
        {
            return Err(invalid("reference image differs from native inspection"));
        }
        if super::published_address(output(&case_root, cases.address_output)?.text()?)
            .map_err(io::Error::other)?
            != cases.address
            || super::media_address(output(&case_root, cases.media_output)?.text()?)
                .map_err(io::Error::other)?
                != cases.media
            || super::callback_address(output(&case_root, cases.callback_output)?.text()?)
                .map_err(io::Error::other)?
                != cases.callback
        {
            return Err(invalid("reference addresses differ from native inspection"));
        }
        tools(&case_root, &cases.tools)?;
        for (case, index) in ReferenceCase::ALL.into_iter().zip(cases.cases) {
            let observed = output(&case_root, index)?;
            let stdout = std::str::from_utf8(&observed.stdout).map_err(io::Error::other)?;
            case.validate_execution(stdout).map_err(io::Error::other)?;
        }
        let reference =
            super::resources::Reference::new(&cases.owner, case_root.join("reference.cid"), &plan)?;
        cleanup(
            &case_root,
            &self.cases.cleanup,
            &cases.owner,
            &cases.id,
            image,
            reference.name(),
        )?;
        let sipp = &self.sipp.run;
        if sipp.directory
            != original_source
                .parent()
                .ok_or_else(|| invalid("original reference source has no parent"))?
                .join("state/sipp")
            || sipp.http.port() == 0
            || sipp.sip.port() == 0
            || !sipp.http.ip().is_loopback()
        {
            return Err(invalid(
                "reference SIPp execution paths or addresses differ",
            ));
        }
        let sipp_root = root.join("sipp");
        let sipp_order = [
            sipp.tools.asterisk,
            sipp.tools.sipp,
            sipp.local_output,
            sipp.callback_output,
            sipp.statistics[0],
            sipp.version,
            sipp.configuration,
            sipp.statistics[1],
            self.sipp.cleanup.inspection,
            self.sipp.cleanup.removal,
            self.sipp.cleanup.absence,
        ];
        if !sipp_order.windows(2).all(|pair| pair[0] < pair[1])
            || super::resources::container_id(&read(&sipp_root.join("reference.cid"), 65)?)?
                != sipp.id
        {
            return Err(invalid(
                "reference SIPp output order or container ID differs",
            ));
        }
        if super::media_address(output(&sipp_root, sipp.local_output)?.text()?)
            .map_err(io::Error::other)?
            != sipp.local
            || super::callback_address(output(&sipp_root, sipp.callback_output)?.text()?)
                .map_err(io::Error::other)?
                != sipp.callback
        {
            return Err(invalid("SIPp addresses differ from native inspection"));
        }
        let sipp_plan = super::Interop::new(
            &sipp.owner,
            sipp.directory
                .join("asterisk")
                .to_str()
                .ok_or_else(|| invalid("SIPp configuration path is not UTF-8"))?,
        )
        .map_err(io::Error::other)?;
        tools(&sipp_root, &sipp.tools)?;
        for index in sipp.statistics {
            let observed = output(&sipp_root, index)?;
            if !observed.stderr.is_empty() {
                return Err(invalid("reference SIPp statistics produced diagnostics"));
            }
            super::validate_sipp_statistics(&observed.stdout).map_err(io::Error::other)?;
        }
        output(&sipp_root, sipp.version)?.exact(format!("rusterisk {version}\n").as_bytes())?;
        output(&sipp_root, sipp.configuration)?.exact(b"Configuration valid.\n")?;
        let stdout = read(&sipp_root.join("native/native.stdout"), 1024 * 1024)?;
        if !read(&sipp_root.join("native/native.stderr"), 1024 * 1024)?.is_empty()
            || !crate::server_distribution::check_server_ready_log(&stdout, sipp.http)?
        {
            return Err(invalid("reference native server readiness differs"));
        }
        let reference = super::resources::Reference::new(
            &sipp.owner,
            sipp_root.join("reference.cid"),
            &sipp_plan,
        )?;
        cleanup(
            &sipp_root,
            &self.sipp.cleanup,
            &sipp.owner,
            &sipp.id,
            image,
            reference.name(),
        )?;
        self.check_configuration(root, captured_source)
    }
}
