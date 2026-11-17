use super::{
    check_sdk_consumer_lock, check_sdk_consumer_metadata, sdk_consumer_run as io,
    SdkConsumerAction, SdkConsumerPhase, SdkConsumerPlan, SdkConsumerRunError, SdkConsumerScope,
    SdkContents, SdkPackage,
};
use std::path::Path;

pub(crate) fn check_sdk_consumer_results(
    contents: &SdkContents,
    artifacts: &Path,
    plan: &SdkConsumerPlan,
) -> Result<(), SdkConsumerRunError> {
    if io::read(&artifacts.join("SOURCE.sha256"), 65)?
        != format!("{}\n", contents.source_digest()).as_bytes()
    {
        return Err(std::io::Error::other("SDK consumer output source differs").into());
    }
    let sums: String = SdkPackage::ALL
        .iter()
        .map(|package| {
            format!(
                "{}  {}-{}.crate\n",
                contents.checksum(*package),
                package.name(),
                contents.version()
            )
        })
        .collect();
    if io::read(&artifacts.join("SHA256SUMS"), 1024)? != sums.as_bytes() {
        return Err(std::io::Error::other("SDK consumer candidate archives differ").into());
    }
    for scope in SdkConsumerScope::ALL {
        let lock = io::read(
            &artifacts
                .join("consumers")
                .join(scope.label())
                .join("Cargo.lock"),
            2 * 1024 * 1024,
        )?;
        check_sdk_consumer_lock(scope, contents.version(), io::seed(contents, scope)?, &lock)
            .map_err(SdkConsumerRunError::Lock)?;
        for action in [
            SdkConsumerAction::Resolve,
            SdkConsumerAction::Metadata,
            SdkConsumerAction::Build,
            SdkConsumerAction::Run,
        ] {
            let phase = SdkConsumerPhase::new(scope, action);
            let output = io::read(&artifacts.join(io::stdout(phase)), 8 * 1024 * 1024)?;
            io::read(&artifacts.join(io::stderr(phase)), 8 * 1024 * 1024)?;
            if matches!(
                action,
                SdkConsumerAction::Resolve | SdkConsumerAction::Metadata
            ) {
                check_sdk_consumer_metadata(plan, scope, contents.version(), &output)
                    .map_err(SdkConsumerRunError::Metadata)?;
            }
        }
        io::check_run_output(artifacts, scope)?;
    }
    Ok(())
}
