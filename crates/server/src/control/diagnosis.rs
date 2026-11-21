use super::{endpoint_resource, Control, ControlError};
use rusterisk_api::{
    diagnosis::{ChannelDiagnosis, Disposition, RouteDiagnosis, RouteStep},
    ChannelId,
};
use rusterisk_config::{expand_variables, Application, ExpansionError};
use std::collections::BTreeSet;

#[derive(Debug)]
struct ExpansionLimit;

fn resolve(value: &str, extension: &str) -> Result<Option<String>, ExpansionLimit> {
    match expand_variables(value, |name| {
        (name == "EXTEN").then(|| extension.to_owned()).ok_or(())
    }) {
        Ok(value) => Ok((!value.contains("${")).then_some(value)),
        Err(ExpansionError::Capacity) => Err(ExpansionLimit),
        Err(ExpansionError::Syntax(_) | ExpansionError::Resolve(())) => Ok(None),
    }
}

impl Control {
    pub(crate) fn diagnose_channel(
        &self,
        id: &ChannelId,
    ) -> Result<ChannelDiagnosis, ControlError> {
        let entry = self
            .channels
            .get(id)
            .ok_or(ControlError::NotFound("Channel not found"))?;
        use rusterisk_api::diagnosis::HoldSource;
        let hold_source = match (
            entry.call.media() == rusterisk_core::MediaState::Held,
            !entry.call.peer_direction().can_receive(),
        ) {
            (true, true) => Some(HoldSource::Both),
            (true, false) => Some(HoldSource::Server),
            (false, true) => Some(HoldSource::Peer),
            (false, false) => None,
        };
        Ok(ChannelDiagnosis {
            id: id.clone(),
            generation: entry.call.generation(),
            creationtime: entry.snapshot.creationtime.clone(),
            state: entry.snapshot.state,
            held: hold_source.is_some(),
            hold_source,
            endpoint: entry.endpoint.clone(),
        })
    }

    pub(crate) fn diagnose_extension(&self, context: String, extension: String) -> RouteDiagnosis {
        let channels = self
            .channels
            .iter()
            .filter(|(_, entry)| {
                entry.snapshot.dialplan.context == context
                    && entry.snapshot.dialplan.exten == extension
            })
            .map(|(id, _)| id.clone())
            .collect();
        let mut result = RouteDiagnosis {
            context,
            extension,
            channels,
            steps: Vec::new(),
            disposition: Disposition::Missing,
            application: None,
            destinations: Vec::new(),
        };
        result.disposition = self.trace_route(&mut result).unwrap_or(Disposition::Limit);
        result
    }

    fn trace_route(&self, result: &mut RouteDiagnosis) -> Result<Disposition, ExpansionLimit> {
        let mut site = result.context.clone();
        let mut number = result.extension.clone();
        let mut priority = 1;
        let mut seen = BTreeSet::new();
        Ok(loop {
            let Some(index) = self.dialplan_program.position(&site, &number, priority) else {
                break if result.steps.is_empty() {
                    Disposition::Missing
                } else {
                    Disposition::End
                };
            };
            if !seen.insert((index, number.clone())) {
                break Disposition::Loop;
            }
            if seen.len() > 128 {
                break Disposition::Limit;
            }
            let step = &self.dialplan_program.steps[usize::from(index)];
            let operation = match step.application {
                Application::NoOp(_) => "NoOp",
                Application::Set { .. } => "Set",
                Application::Answer => "Answer",
                Application::Dial { .. } => "Dial",
                Application::Stasis { .. } => "Stasis",
                Application::Playback(_) => "Playback",
                Application::Wait(_) => "Wait",
                Application::Goto { .. } => "Goto",
                Application::Hangup => "Hangup",
            };
            result.steps.push(RouteStep {
                context: site.clone(),
                pattern: step.extension.clone(),
                priority,
                operation: operation.to_owned(),
            });
            match &step.application {
                Application::Stasis { application, .. } => {
                    result.application = resolve(application, &number)?;
                    break Disposition::Stasis;
                }
                Application::Dial { destinations, .. } => {
                    for destination in destinations {
                        let target = resolve(destination, &number)?;
                        let endpoint = target
                            .as_deref()
                            .and_then(|target| endpoint_resource(target).ok());
                        result.destinations.push(self.endpoints.diagnose(endpoint));
                    }
                    break Disposition::Dial;
                }
                Application::Goto {
                    context,
                    extension,
                    priority: next,
                } => {
                    let Some(next_site) = resolve(context.as_deref().unwrap_or(&site), &number)?
                    else {
                        break Disposition::Dynamic;
                    };
                    let Some(next_number) =
                        resolve(extension.as_deref().unwrap_or(&number), &number)?
                    else {
                        break Disposition::Dynamic;
                    };
                    site = next_site;
                    number = next_number;
                    priority = *next;
                }
                Application::Hangup => break Disposition::Hangup,
                Application::NoOp(_)
                | Application::Set { .. }
                | Application::Answer
                | Application::Playback(_)
                | Application::Wait(_) => {
                    let Some(next) = priority.checked_add(1) else {
                        break Disposition::End;
                    };
                    priority = next;
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn static_route_values_respect_the_dialplan_expansion_boundary() -> Result<(), ExpansionLimit> {
        let limit = "7".repeat(4096);
        assert_eq!(
            resolve("${EXTEN}", &limit)?.as_deref(),
            Some(limit.as_str())
        );
        assert!(resolve("x${EXTEN}", &limit).is_err());
        assert!(resolve("${CALLERID(num)}", "201")?.is_none());
        assert_eq!(
            resolve("PJSIP/${EXTEN}", "201")?.as_deref(),
            Some("PJSIP/201")
        );
        Ok(())
    }
}
