use crate::TaskError;
use xtask::reference::{native_server_build, Interop, ReferenceCase};

pub fn execute(execute: bool) -> Result<(), TaskError> {
    let root = std::env::current_dir()?;
    if execute {
        #[cfg(unix)]
        {
            let captured = xtask::release_check::CapturedReleaseSource::read(&root)
                .map_err(|error| TaskError::Contract(error.to_string()))?;
            let image = xtask::reference::ReferenceImage::inspect(&root)?;
            captured
                .verify_reference(&image)
                .map_err(|error| TaskError::Contract(error.to_string()))?;
            return Ok(());
        }
        #[cfg(not(unix))]
        return Err(TaskError::Process(
            xtask::process::ProcessError::UnsupportedSupervision,
        ));
    }
    let identity = super::execution_identity(false)?;
    let configuration = root
        .join("target/interop")
        .join(format!("{identity}.configuration"));
    let configuration = configuration
        .to_str()
        .ok_or_else(|| TaskError::Contract("reference configuration path isn't Unicode".into()))?;
    let plan = Interop::new(&identity, configuration)
        .map_err(|error| TaskError::Contract(error.to_string()))?;
    let build = native_server_build();
    println!("preflight: node-check");
    println!(
        "budget={}s cargo {}",
        build.budget.as_secs(),
        build.arguments.join(" ")
    );
    println!("SIPp: authenticated registration and removal with Asterisk and Rusterisk");
    for step in [
        plan.start(),
        plan.port(),
        plan.media_address(),
        plan.callback_address(),
    ] {
        println!(
            "budget={}s docker {}",
            step.budget.as_secs(),
            step.arguments.join(" ")
        );
    }
    for case in ReferenceCase::ALL {
        println!(
            "budget={}s cargo {}",
            case.budget().as_secs(),
            case.cargo_arguments().join(" ")
        );
    }
    let cleanup = plan.cleanup();
    println!(
        "budget={}s docker {}",
        cleanup.budget.as_secs(),
        cleanup.arguments.join(" ")
    );
    Ok(())
}
