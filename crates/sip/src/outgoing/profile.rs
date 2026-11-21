use crate::{
    registrar::{BindingIdentity, Bindings},
    transport::UriSecurity,
    Destination, ReferAuthority, SignalingRoute, SipError, StaticTarget, StreamFlowId,
};
use rsipstack::sip::Uri;
use rusterisk_config::{Auth, Config, Endpoint, Transport};
use std::net::SocketAddr;

const MAX_VALUE_BYTES: usize = 4096;
const MAX_REFERENCES: usize = 4096;
const MAX_CREDENTIALS: usize = 8;

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct DialResource<'a> {
    endpoint: &'a str,
    user: Option<&'a str>,
}

impl<'a> DialResource<'a> {
    pub fn parse(resource: &'a str) -> Result<Self, SipError> {
        let (user, endpoint) = match resource.split_once('@') {
            Some((user, endpoint))
                if !user.is_empty()
                    && user.len() <= 128
                    && user
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || b"+*-#._".contains(&byte)) =>
            {
                (Some(user), endpoint)
            }
            Some(_) => return Err(SipError::InvalidPacket),
            None => (None, resource),
        };
        if endpoint.is_empty() || endpoint.len() > 128 || endpoint.contains('@') {
            return Err(SipError::InvalidPacket);
        }
        Ok(Self { endpoint, user })
    }

    pub fn endpoint(self) -> &'a str {
        self.endpoint
    }
}

#[derive(Clone)]
pub struct OutboundProfile {
    pub(super) endpoint: Endpoint,
    user: Option<String>,
    pub(super) transport: Transport,
    pub(super) credentials: Vec<Auth>,
    pub(super) authority: ReferAuthority,
    contact: Option<Uri>,
}

impl core::fmt::Debug for OutboundProfile {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("OutboundProfile")
            .field("endpoint", &self.endpoint.name)
            .field("transport", &self.transport.name)
            .finish_non_exhaustive()
    }
}

pub struct PreparedOutbound {
    selected: SelectedOutbound,
    destination: Destination,
}

pub(super) enum RouteSelection {
    RegisteredFirst,
    Static,
}

enum SelectedRoute {
    Registered(BindingIdentity),
    Static,
}

pub(super) struct SelectedOutbound {
    pub(super) profile: OutboundProfile,
    pub(super) uri: Uri,
    pub(super) security: UriSecurity,
    route: SelectedRoute,
}

impl OutboundProfile {
    pub fn bind_address(
        mut self,
        requested: SocketAddr,
        bound: SocketAddr,
    ) -> Result<Self, SipError> {
        if self.transport.bind != requested
            || bound.port() == 0
            || bound.ip().is_multicast()
            || requested.is_ipv4() != bound.is_ipv4()
            || (!requested.ip().is_unspecified() && requested.ip() != bound.ip())
            || (requested.port() != 0 && requested.port() != bound.port())
        {
            return Err(SipError::InvalidPacket);
        }
        self.transport.bind = bound;
        Ok(self)
    }

    pub fn capture(config: &Config, resource: &str) -> Result<Self, SipError> {
        Self::for_resource(config, DialResource::parse(resource)?)
    }

    pub fn for_resource(config: &Config, dial: DialResource<'_>) -> Result<Self, SipError> {
        let endpoint = config
            .endpoints
            .iter()
            .find(|candidate| candidate.name == dial.endpoint)
            .ok_or(SipError::InvalidPacket)?;
        let transport = config
            .transports
            .iter()
            .find(|transport| transport.name == endpoint.transport)
            .ok_or(SipError::InvalidPacket)?;
        check_settings(endpoint, transport)?;
        let mut credentials = Vec::with_capacity(endpoint.outbound_auth.len());
        for name in &endpoint.outbound_auth {
            let auth = config
                .auth
                .iter()
                .find(|auth| &auth.name == name)
                .ok_or(SipError::InvalidPacket)?;
            for value in [&auth.name, &auth.username, &auth.realm] {
                check_value(value)?;
            }
            check_value(auth.password.expose())?;
            credentials.push(auth.clone());
        }
        let contact = config
            .aors
            .iter()
            .filter(|aor| endpoint.aors.contains(&aor.name))
            .find_map(|aor| aor.contact.as_deref())
            .map(|contact| {
                if contact.len() > 2048 {
                    return Err(SipError::Capacity);
                }
                Uri::try_from(contact).map_err(SipError::from)
            })
            .transpose()?;
        Ok(Self {
            endpoint: endpoint.clone(),
            user: dial.user.map(str::to_owned),
            transport: transport.clone(),
            credentials,
            authority: ReferAuthority::configured_peer(endpoint),
            contact,
        })
    }

    pub(super) fn dial_resource(&self) -> DialResource<'_> {
        DialResource {
            endpoint: &self.endpoint.name,
            user: self.user.as_deref(),
        }
    }

    pub(super) fn select(
        self,
        contacts: &Bindings,
        selection: RouteSelection,
        now: u64,
    ) -> Result<SelectedOutbound, SipError> {
        let registered = match selection {
            RouteSelection::RegisteredFirst => contacts.best(&self.endpoint.aors, now),
            RouteSelection::Static => None,
        };
        let (mut uri, route) = match registered {
            Some(binding) => (
                binding.contact.uri().clone(),
                SelectedRoute::Registered(binding.identity()),
            ),
            None => (
                self.contact.clone().ok_or(SipError::InvalidPacket)?,
                SelectedRoute::Static,
            ),
        };
        if let Some(user) = &self.user {
            uri.auth = Some(rsipstack::sip::Auth {
                user: user.clone(),
                password: None,
            });
        }
        let security = UriSecurity::target(self.transport.protocol, &uri)?;
        Ok(SelectedOutbound {
            profile: self,
            uri,
            security,
            route,
        })
    }
}

impl SelectedOutbound {
    pub(super) fn prepare(self) -> Result<PreparedOutbound, SipError> {
        let destination = match &self.route {
            SelectedRoute::Registered(binding) => Destination::Registered(binding.route.peer),
            SelectedRoute::Static => Destination::Static(StaticTarget::from_uri(
                self.profile.transport.protocol,
                &self.uri.to_string(),
            )?),
        };
        Ok(PreparedOutbound {
            selected: self,
            destination,
        })
    }

    pub(super) fn route(
        &self,
        contacts: &Bindings,
        resolved: Option<SocketAddr>,
        flow: Option<StreamFlowId>,
        now: u64,
    ) -> Result<SignalingRoute, SipError> {
        let route = match (&self.route, resolved) {
            (SelectedRoute::Registered(binding), None)
                if binding.route.flow == flow && contacts.contains(binding, now) =>
            {
                binding.route
            }
            (SelectedRoute::Registered(_), _) => return Err(SipError::InvalidPacket),
            (SelectedRoute::Static, Some(peer)) => SignalingRoute { peer, flow },
            (SelectedRoute::Static, None) => SignalingRoute {
                peer: self.uri.host_with_port.to_string().parse::<SocketAddr>()?,
                flow,
            },
        };
        let peer = route.peer;
        if peer.port() == 0 || peer.ip().is_unspecified() || peer.ip().is_multicast() {
            return Err(SipError::InvalidPacket);
        }
        Ok(route)
    }
}

impl PreparedOutbound {
    pub fn destination(&self) -> &Destination {
        &self.destination
    }

    /// Returns the captured registration's stream flow.
    /// Static destinations and legacy bindings return `None`.
    pub fn flow(&self) -> Option<StreamFlowId> {
        match &self.selected.route {
            SelectedRoute::Registered(binding) => binding.route.flow,
            SelectedRoute::Static => None,
        }
    }

    pub(super) fn into_selected(self) -> SelectedOutbound {
        self.selected
    }
}

fn check_value(value: &str) -> Result<(), SipError> {
    if value.len() > MAX_VALUE_BYTES {
        return Err(SipError::Capacity);
    }
    Ok(())
}

fn check_settings(endpoint: &Endpoint, transport: &Transport) -> Result<(), SipError> {
    if endpoint.outbound_auth.len() > MAX_CREDENTIALS
        || endpoint.aors.len() > MAX_REFERENCES
        || endpoint.codecs.len() > MAX_REFERENCES
    {
        return Err(SipError::Capacity);
    }
    for references in [&endpoint.aors, &endpoint.outbound_auth] {
        references.iter().try_fold(0usize, |total, value| {
            total
                .checked_add(value.len())
                .filter(|bytes| *bytes <= MAX_VALUE_BYTES)
                .ok_or(SipError::Capacity)
        })?;
    }
    for value in [
        &endpoint.name,
        &endpoint.transport,
        &endpoint.auth,
        &endpoint.context,
        &transport.name,
    ]
    .into_iter()
    .chain(&endpoint.aors)
    .chain(&endpoint.outbound_auth)
    {
        check_value(value)?;
    }
    if let Some(path) = &transport.ca_list_file {
        check_value(path)?;
    }
    if let Some(files) = &transport.tls {
        check_value(&files.certificate)?;
        check_value(&files.private_key)?;
    }
    Ok(())
}
