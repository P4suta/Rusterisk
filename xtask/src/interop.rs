use crate::TaskError;
use std::{io::Read, path::Path, process::Command, time::Duration};
use xtask::{
    process::{run, ProcessError},
    reference::{
        callback_address, copy_configuration, media_address, published_address, Interop, Step,
    },
};

fn perform(step: &Step) -> Result<(), ProcessError> {
    run(Command::new("docker").args(&step.arguments), step.budget)
}

fn captured(step: &Step, identity: &str, field: &str) -> Result<String, ProcessError> {
    let directory = Path::new("target/interop");
    std::fs::create_dir_all(directory)?;
    let path = directory.join(format!("{identity}.{field}"));
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)?;
    let result = (|| {
        run(
            Command::new("docker").args(&step.arguments).stdout(file),
            step.budget,
        )?;
        let mut output = String::with_capacity(1025);
        std::fs::File::open(&path)?
            .take(1025)
            .read_to_string(&mut output)?;
        Ok(output)
    })();
    if let Err(error) = std::fs::remove_file(path) {
        if let Err(primary) = &result {
            eprintln!("reference inspection failed: {primary}");
        }
        return Err(ProcessError::Io(error));
    }
    result
}

pub fn execute(execute: bool) -> Result<(), TaskError> {
    let identity = super::execution_identity(execute)?;
    let configuration = std::env::current_dir()?
        .join("target/interop")
        .join(format!("{identity}.configuration"));
    let configuration_path = configuration
        .to_str()
        .ok_or_else(|| TaskError::Contract("reference configuration path isn't Unicode".into()))?;
    let plan = Interop::new(&identity, configuration_path)
        .map_err(|error| TaskError::Contract(error.to_string()))?;
    let start = plan.start();
    let cleanup = plan.cleanup();
    println!("preflight: node-check");
    for step in [
        &start,
        &plan.port(),
        &plan.media_address(),
        &plan.callback_address(),
    ] {
        println!(
            "budget={}s docker {}",
            step.budget.as_secs(),
            step.arguments.join(" ")
        );
    }
    let arguments = [
        "test",
        "--locked",
        "--package",
        "rusterisk-client",
        "--test",
        "reference",
        "--",
        "--ignored",
    ];
    println!("budget=300s cargo {}", arguments.join(" "));
    println!(
        "budget={}s docker {}",
        cleanup.budget.as_secs(),
        cleanup.arguments.join(" ")
    );
    if !execute {
        return Ok(());
    }
    super::node_check()?;
    std::fs::create_dir_all("target/interop")?;
    copy_configuration(Path::new("tests/interop/asterisk"), &configuration)?;
    let result = (|| {
        std::fs::create_dir(configuration.join("sounds"))?;
        perform(&start)?;
        let address = published_address(&captured(&plan.port(), &identity, "port")?)
            .map_err(|_| ProcessError::InvalidOutput)?;
        let media = media_address(&captured(&plan.media_address(), &identity, "media")?)
            .map_err(|_| ProcessError::InvalidOutput)?;
        let callback_output = captured(&plan.callback_address(), &identity, "callback")?;
        let callback = callback_address(&callback_output).map_err(|_| {
            eprintln!("invalid reference callback inspection: {callback_output:?}");
            ProcessError::InvalidOutput
        })?;
        println!("reference callback address: {callback}");
        run(
            Command::new("cargo")
                .args(arguments)
                .env("RUSTERISK_REFERENCE_URL", format!("http://{address}"))
                .env("RUSTERISK_REFERENCE_MEDIA_IP", media.to_string())
                .env("RUSTERISK_REFERENCE_CALLBACK_IP", callback.to_string())
                .env("RUSTERISK_REFERENCE_CONFIGURATION", &configuration),
            Duration::from_secs(300),
        )
    })();
    if result.is_err() {
        if let Err(error) = perform(&plan.logs()) {
            eprintln!("reference logs failed: {error}");
        }
    }
    let result = match (result, perform(&cleanup)) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(operation), Err(cleanup)) => Err(TaskError::ReferenceCleanup { operation, cleanup }),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(TaskError::Process(error)),
    };
    match (result, std::fs::remove_dir_all(configuration)) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(primary), Err(cleanup)) => Err(TaskError::Contract(format!(
            "{primary}; reference configuration cleanup failed: {cleanup}"
        ))),
        (Err(error), Ok(())) => Err(error),
        (Ok(()), Err(error)) => Err(TaskError::Io(error)),
    }
}
