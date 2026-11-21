use super::{Control, ControlError};
use rusterisk_api::{Endpoint, EndpointState};
use rusterisk_config::Config;
use rusterisk_core::{ContactPresence, ResourceKind, ResourceSource};
use std::collections::BTreeMap;

const INITIAL_SOURCE: u64 = 1_u64 << 32;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EndpointKey {
    pub resource: String,
    pub generation: u64,
}

struct Route {
    source: u64,
    static_contact: bool,
    contact: ContactPresence,
}

#[derive(Clone, Copy)]
struct SourceSerial(u64);

impl SourceSerial {
    fn next(&mut self) -> Result<u64, ControlError> {
        let next = self.0.checked_add(1).ok_or(ControlError::Capacity)?;
        self.0 = next;
        Ok(next)
    }
}

pub(super) struct Endpoints {
    generation: u64,
    source_serial: SourceSerial,
    routes: BTreeMap<String, Route>,
}

impl Default for Endpoints {
    fn default() -> Self {
        Self {
            generation: 1,
            source_serial: SourceSerial(INITIAL_SOURCE),
            routes: BTreeMap::new(),
        }
    }
}

impl Endpoints {
    pub(super) fn diagnose(&self, resource: Option<&str>) -> rusterisk_api::diagnosis::Destination {
        use rusterisk_api::diagnosis::{Destination, Registration};
        let route = resource.and_then(|resource| self.routes.get(resource));
        Destination {
            endpoint: resource.map(str::to_owned),
            static_contact: route.map(|r| r.static_contact),
            registration: route.map_or(Registration::Unknown, |r| match r.contact {
                ContactPresence::Present => Registration::Present,
                ContactPresence::Absent => Registration::Absent,
            }),
            available: route
                .map(|r| r.static_contact || r.contact == ContactPresence::Present)
                .or(resource.map(|_| false)),
        }
    }

    pub(super) fn configured(config: &Config) -> Self {
        let routes: BTreeMap<_, _> = config
            .endpoints
            .iter()
            .enumerate()
            .map(|(index, endpoint)| {
                let static_contact = endpoint.aors.iter().any(|name| {
                    config
                        .aors
                        .iter()
                        .any(|aor| aor.name == *name && aor.contact.is_some())
                });
                (
                    endpoint.name.clone(),
                    Route {
                        source: INITIAL_SOURCE + index as u64 + 1,
                        static_contact,
                        contact: ContactPresence::Absent,
                    },
                )
            })
            .collect();
        let source_serial = SourceSerial(
            routes
                .values()
                .map(|route| route.source)
                .max()
                .unwrap_or(INITIAL_SOURCE),
        );
        Self {
            generation: 1,
            source_serial,
            routes,
        }
    }

    pub(super) fn prepare_configuration(&self, config: &Config) -> Result<Self, ControlError> {
        let generation = self
            .generation
            .checked_add(1)
            .ok_or(ControlError::Capacity)?;
        let mut replacement = Self::configured(config);
        replacement.generation = generation;
        replacement.source_serial = self.source_serial;
        for (name, route) in &mut replacement.routes {
            if let Some(current) = self.routes.get(name) {
                route.source = current.source;
                route.contact = current.contact;
            } else {
                route.source = replacement.source_serial.next()?;
            }
        }
        Ok(replacement)
    }

    pub(super) fn preserve_contacts(&mut self, current: &Self) {
        for (name, route) in &mut self.routes {
            if let Some(current) = current
                .routes
                .get(name)
                .filter(|current| current.source == route.source)
            {
                route.contact = current.contact;
            }
        }
    }

    pub(super) fn source(&self, resource: &str) -> Option<ResourceSource> {
        self.routes
            .get(resource)
            .map(|route| ResourceSource::Specific {
                kind: ResourceKind::Endpoint,
                generation: route.source,
            })
    }

    pub(super) fn source_names(&self, generation: u64) -> impl Iterator<Item = String> + '_ {
        let technology = (generation == 0 && !self.routes.is_empty()).then(|| "PJSIP".to_owned());
        technology.into_iter().chain(
            self.routes
                .iter()
                .filter(move |(_, route)| route.source == generation)
                .map(|(name, _)| format!("PJSIP/{name}")),
        )
    }

    pub(super) fn has_technology(&self, technology: &str) -> bool {
        technology == "PJSIP" && !self.routes.is_empty()
    }

    pub(super) fn observe(&mut self, key: &EndpointKey, contact: ContactPresence) -> bool {
        if key.generation == self.generation {
            if let Some(route) = self.routes.get_mut(&key.resource) {
                let changed = !route.static_contact && route.contact != contact;
                route.contact = contact;
                return changed;
            }
        }
        false
    }
}

impl Control {
    pub fn endpoint_key(&self, resource: &str) -> Result<EndpointKey, ControlError> {
        if !self.endpoints.routes.contains_key(resource) {
            return Err(ControlError::NotFound("Endpoint not found"));
        }
        Ok(EndpointKey {
            resource: resource.into(),
            generation: self.endpoints.generation,
        })
    }

    pub(super) fn endpoint(
        &self,
        technology: &str,
        resource: &str,
    ) -> Result<Endpoint, ControlError> {
        if !technology.eq_ignore_ascii_case("PJSIP") {
            return Err(ControlError::NotFound("Endpoint not found"));
        }
        let route = self
            .endpoints
            .routes
            .get(resource)
            .ok_or(ControlError::NotFound("Endpoint not found"))?;
        let available = route.static_contact || route.contact == ContactPresence::Present;
        let channel_ids = self
            .channels
            .iter()
            .filter(|(_, channel)| channel.endpoint == resource)
            .map(|(id, _)| id.clone())
            .collect();
        Ok(Endpoint {
            technology: "PJSIP".into(),
            resource: resource.into(),
            state: Some(if available {
                EndpointState::Online
            } else {
                EndpointState::Offline
            }),
            channel_ids,
        })
    }

    pub(super) fn endpoint_list(
        &self,
        technology: Option<&str>,
    ) -> Result<Vec<Endpoint>, ControlError> {
        if let Some(technology) = technology {
            if technology != "PJSIP" || self.endpoints.routes.is_empty() {
                return Err(ControlError::EndpointTechnology(technology.into()));
            }
        }
        self.endpoints
            .routes
            .keys()
            .map(|resource| self.endpoint("PJSIP", resource))
            .collect()
    }
}

#[cfg(test)]
#[path = "reload_tests.rs"]
mod reload_tests;
