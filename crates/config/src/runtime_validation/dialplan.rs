use super::{invalid, list, valid_dial_destinations, value};
use crate::{
    valid_name, valid_variable_expression, validate_expansion, Application, ConfigError, Dialplan,
};
use std::collections::BTreeSet;

const FILE: &str = "extensions.conf";

fn expansion(text: &str) -> Result<(), ConfigError> {
    value(FILE, "application value", text)?;
    validate_expansion(text).map_err(|reason| invalid(FILE, reason))
}

fn application(application: &Application) -> Result<(), ConfigError> {
    match application {
        Application::NoOp(text) => expansion(text),
        Application::Set { variable, value } => {
            if !valid_variable_expression(variable) {
                return Err(invalid(FILE, "invalid Set variable"));
            }
            expansion(value)
        }
        Application::Dial {
            destinations,
            timeout,
        } => {
            if !valid_dial_destinations(destinations) || *timeout == Some(0) {
                return Err(invalid(FILE, "invalid Dial destinations or timeout"));
            }
            for destination in destinations {
                expansion(destination)?;
            }
            list(FILE, destinations.iter().map(String::as_str))
        }
        Application::Stasis {
            application,
            arguments,
        } => {
            if !valid_name(application) {
                return Err(invalid(FILE, "invalid Stasis application"));
            }
            list(FILE, arguments.iter().map(String::as_str))?;
            for argument in arguments {
                expansion(argument)?;
            }
            Ok(())
        }
        Application::Playback(name) => {
            if valid_name(name) {
                Ok(())
            } else {
                Err(invalid(FILE, "invalid Playback name"))
            }
        }
        Application::Goto {
            context,
            extension,
            priority: _,
        } => {
            if context.is_some() && extension.is_none() {
                return Err(invalid(FILE, "Goto context requires an extension"));
            }
            for text in [context, extension].into_iter().flatten() {
                expansion(text)?;
            }
            Ok(())
        }
        Application::Answer | Application::Wait(_) | Application::Hangup => Ok(()),
    }
}

pub(super) fn validate(program: &Dialplan) -> Result<(), ConfigError> {
    if program.steps.len() > 4096 {
        return Err(invalid(FILE, "dialplan step capacity exceeded"));
    }
    let mut seen = BTreeSet::new();
    for step in &program.steps {
        if !valid_name(&step.context) || step.priority == 0 {
            return Err(invalid(FILE, "invalid dialplan context or priority"));
        }
        value(FILE, "extension", &step.extension)?;
        if !seen.insert((
            step.context.as_str(),
            step.extension.as_str(),
            step.priority,
        )) {
            return Err(invalid(FILE, "duplicate dialplan priority"));
        }
        application(&step.application)?;
    }
    Ok(())
}
