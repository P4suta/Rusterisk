use crate::TaskError;
use std::{io::Read, path::Path, process::Command, time::Duration};
use xtask::{
    process::{CommandSupervisor, ProcessError},
    reference::{
        callback_address, copy_configuration, media_address, published_address, Interop,
        ReferenceCase, ReferenceImage, SipTransferCase, Step,
    },
};

const SERVER_BUILD: [&str; 10] = [
    "build",
    "--locked",
    "--package",
    "rusterisk-server",
    "--bin",
    "rusterisk",
    "--target-dir",
    "target",
    "--jobs",
    "1",
];

fn perform(supervisor: &mut CommandSupervisor, step: &Step) -> Result<(), ProcessError> {
    supervisor.run(Command::new("docker").args(&step.arguments), step.budget)
}

fn captured(
    supervisor: &mut CommandSupervisor,
    step: &Step,
    identity: &str,
    field: &str,
) -> Result<String, ProcessError> {
    captured_command(
        supervisor,
        Command::new("docker").args(&step.arguments),
        step.budget,
        identity,
        field,
        1025,
    )
}

fn captured_command(
    supervisor: &mut CommandSupervisor,
    command: &mut Command,
    budget: Duration,
    identity: &str,
    field: &str,
    limit: u64,
) -> Result<String, ProcessError> {
    let directory = Path::new("target/interop");
    std::fs::create_dir_all(directory)?;
    let path = directory.join(format!("{identity}.{field}"));
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)?;
    let operation = supervisor.run(command.stdout(file), budget);
    let output = (|| {
        let mut output = String::new();
        std::fs::File::open(&path)?
            .take(limit)
            .read_to_string(&mut output)?;
        Ok::<_, std::io::Error>(output)
    })();
    if operation.is_err() {
        if let Ok(output) = &output {
            eprintln!("{output}");
        }
    }
    let result = match (operation, output) {
        (Ok(()), Ok(output)) => Ok(output),
        (Err(error), Ok(_)) => Err(error),
        (Ok(()), Err(error)) => Err(ProcessError::Io(error)),
        (Err(operation), Err(cleanup)) => Err(ProcessError::Cleanup {
            operation: operation.to_string(),
            cleanup: cleanup.to_string(),
        }),
    };
    match (result, std::fs::remove_file(path)) {
        (Ok(output), Ok(())) => Ok(output),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(error)) => Err(ProcessError::Io(error)),
        (Err(operation), Err(cleanup)) => Err(ProcessError::Cleanup {
            operation: operation.to_string(),
            cleanup: cleanup.to_string(),
        }),
    }
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
    println!("budget=300s cargo {}", SERVER_BUILD.join(" "));
    println!("SIPp: authenticated registration and removal with Asterisk and Rusterisk");
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
    for case in ReferenceCase::ALL {
        println!(
            "budget={}s cargo {}",
            case.budget().as_secs(),
            case.cargo_arguments().join(" ")
        );
    }
    println!(
        "budget={}s docker {}",
        cleanup.budget.as_secs(),
        cleanup.arguments.join(" ")
    );
    if !execute {
        return Ok(());
    }
    let mut supervisor = CommandSupervisor::new().map_err(TaskError::Process)?;
    let result = (|| {
        super::node_check_with(|program, arguments| {
            println!("check: {program} {}", arguments.join(" "));
            supervisor
                .run(
                    Command::new(program).args(arguments),
                    Duration::from_secs(180),
                )
                .map_err(TaskError::Process)
        })?;
        supervisor
            .run(
                Command::new("cargo").args(SERVER_BUILD),
                Duration::from_secs(300),
            )
            .map_err(TaskError::Process)?;
        let inspected = captured(
            &mut supervisor,
            &ReferenceImage::inspection(),
            &identity,
            "reference-image",
        )
        .map_err(TaskError::Process)?;
        let image = ReferenceImage::parse_inspection(&inspected)
            .map_err(|error| TaskError::Contract(error.to_string()))?;
        #[cfg(unix)]
        {
            let root = std::env::current_dir()?;
            let comparison = image.compare_sipp_registrations_with(
                &root,
                &root.join("target/debug/rusterisk"),
                &mut supervisor,
            )?;
            println!(
                "SIPp completed calls: Asterisk={}, Rusterisk={}",
                comparison.completed_calls(xtask::reference::SippAdapter::Asterisk),
                comparison.completed_calls(xtask::reference::SippAdapter::Rusterisk),
            );
        }
        execute_owned(&mut supervisor, &plan, &image, &configuration, &identity)
    })();
    match (result, supervisor.finish()) {
        (result, Ok(())) => result,
        (Ok(()), Err(error)) => Err(TaskError::Process(error)),
        (Err(operation), Err(cleanup)) => Err(TaskError::Contract(format!(
            "{operation}; interop supervisor cleanup failed: {cleanup}"
        ))),
    }
}

fn execute_owned(
    supervisor: &mut CommandSupervisor,
    plan: &Interop,
    image: &ReferenceImage,
    configuration: &Path,
    identity: &str,
) -> Result<(), TaskError> {
    std::fs::create_dir_all("target/interop")?;
    copy_configuration(Path::new("tests/interop/asterisk"), configuration)?;
    let result: Result<(), ProcessError> = (|| {
        std::fs::create_dir(configuration.join("sounds"))?;
        let start = image.start(plan).map_err(|_| ProcessError::InvalidOutput)?;
        image
            .check_start(&start)
            .map_err(|_| ProcessError::InvalidOutput)?;
        perform(supervisor, &start)?;
        let address = published_address(&captured(supervisor, &plan.port(), identity, "port")?)
            .map_err(|_| ProcessError::InvalidOutput)?;
        let media = media_address(&captured(
            supervisor,
            &plan.media_address(),
            identity,
            "media",
        )?)
        .map_err(|_| ProcessError::InvalidOutput)?;
        let callback_output = captured(supervisor, &plan.callback_address(), identity, "callback")?;
        let callback = callback_address(&callback_output).map_err(|_| {
            eprintln!("invalid reference callback inspection: {callback_output:?}");
            ProcessError::InvalidOutput
        })?;
        println!("reference callback address: {callback}");
        for case in ReferenceCase::ALL {
            let field = match case {
                ReferenceCase::Ari => "ari",
                ReferenceCase::Transfer(SipTransferCase::Blind) => "sip-transfer-blind",
                ReferenceCase::Transfer(SipTransferCase::Attended) => "sip-transfer-attended",
            };
            let output = captured_command(
                supervisor,
                Command::new("cargo")
                    .args(case.cargo_arguments())
                    .env("RUSTERISK_REFERENCE_URL", format!("http://{address}"))
                    .env("RUSTERISK_REFERENCE_MEDIA_IP", media.to_string())
                    .env("RUSTERISK_REFERENCE_CALLBACK_IP", callback.to_string())
                    .env("RUSTERISK_REFERENCE_CONFIGURATION", configuration),
                case.budget(),
                identity,
                field,
                65_537,
            )?;
            print!("{output}");
            case.validate_execution(&output)
                .map_err(|_| ProcessError::InvalidOutput)?;
        }
        Ok(())
    })();
    let mut result = result;
    if result.is_err() {
        let logs = plan.logs();
        if let Err(cleanup) =
            supervisor.run_cleanup(Command::new("docker").args(&logs.arguments), logs.budget)
        {
            if let Err(operation) = result {
                result = Err(ProcessError::Cleanup {
                    operation: operation.to_string(),
                    cleanup: cleanup.to_string(),
                });
            }
        }
    }
    let cleanup = plan.cleanup();
    let result = match (
        result,
        supervisor.run_cleanup(
            Command::new("docker").args(&cleanup.arguments),
            cleanup.budget,
        ),
    ) {
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
