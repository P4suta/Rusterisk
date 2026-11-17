use crate::{encryption::TlsClientContext, ControlError};
use rusterisk_config::{Codec, Config, Dtmf, Rtp, Transport};
use rusterisk_sip::OutboundProfile;
use std::{fmt, net::SocketAddr, num::NonZeroU64};

#[derive(Clone)]
pub struct NativeCallProfile {
    endpoint: String,
    sip: OutboundProfile,
    media: NativeMediaProfile,
    listener: NativeListenerProfile,
}

impl fmt::Debug for NativeCallProfile {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NativeCallProfile")
            .field("endpoint", &self.endpoint)
            .finish_non_exhaustive()
    }
}

#[derive(Clone)]
enum NativeListenerProfile {
    Requested(Transport),
    Bound(NativeTransportBinding),
}

#[derive(Clone)]
pub(crate) struct NativeTransportBinding {
    requested: Transport,
    address: SocketAddr,
    listener: usize,
    configuration: NonZeroU64,
    context: Option<TlsClientContext>,
}

impl NativeTransportBinding {
    pub(crate) fn new(
        requested: Transport,
        address: SocketAddr,
        listener: usize,
        configuration: NonZeroU64,
        context: Option<TlsClientContext>,
    ) -> Self {
        Self {
            requested,
            address,
            listener,
            configuration,
            context,
        }
    }

    pub(crate) fn listener(&self) -> usize {
        self.listener
    }

    pub(crate) fn configuration(&self) -> NonZeroU64 {
        self.configuration
    }

    pub(crate) fn context(&self) -> Option<TlsClientContext> {
        self.context.clone()
    }

    fn accepts(&self, transport: &Transport) -> bool {
        let mut bound = self.requested.clone();
        bound.bind = self.address;
        transport == &self.requested || transport == &bound
    }

    fn same(&self, other: &Self) -> bool {
        self.requested == other.requested
            && self.address == other.address
            && self.listener == other.listener
            && self.configuration == other.configuration
            && match (&self.context, &other.context) {
                (Some(first), Some(second)) => first.same_profile(second),
                (None, None) => true,
                (Some(_), None) | (None, Some(_)) => false,
            }
    }
}

#[derive(Clone)]
pub(crate) struct BoundNativeProfiles {
    bindings: Vec<NativeTransportBinding>,
}

impl BoundNativeProfiles {
    pub(crate) fn new(bindings: Vec<NativeTransportBinding>) -> Self {
        Self { bindings }
    }

    fn find(&self, transport: &Transport) -> Result<&NativeTransportBinding, ControlError> {
        self.bindings
            .iter()
            .find(|binding| binding.accepts(transport))
            .ok_or(ControlError::Conflict(
                "Starting signaling listener changed",
            ))
    }
}

#[derive(Clone)]
pub(crate) struct NativeProfileCatalog {
    config: Config,
    bindings: Option<BoundNativeProfiles>,
}

impl NativeProfileCatalog {
    pub(crate) fn new(config: &Config) -> Self {
        Self {
            config: config.clone(),
            bindings: None,
        }
    }

    pub(crate) fn is_bound_to(&self, bindings: &BoundNativeProfiles) -> Result<bool, ControlError> {
        if self.config.transports.len() != bindings.bindings.len() {
            return Err(ControlError::Conflict(
                "Starting signaling listeners changed",
            ));
        }
        for transport in &self.config.transports {
            let binding = bindings.find(transport)?;
            if let Some(current) = &self.bindings {
                if !current.find(transport)?.same(binding) {
                    return Err(ControlError::Conflict("Starting signaling profile changed"));
                }
            }
        }
        Ok(self.bindings.is_some())
    }

    pub(crate) fn bind(&mut self, bindings: BoundNativeProfiles) -> Result<(), ControlError> {
        if self.is_bound_to(&bindings)? {
            return Ok(());
        }
        for transport in &mut self.config.transports {
            transport.bind = bindings.find(transport)?.address;
        }
        self.bindings = Some(bindings);
        Ok(())
    }

    pub(crate) fn capture(&self, endpoint: &str) -> Result<NativeCallProfile, ControlError> {
        let endpoint = endpoint
            .strip_prefix("PJSIP/")
            .ok_or(ControlError::Unsupported)?;
        NativeCallProfile::capture(&self.config, self.bindings.as_ref(), endpoint)
    }
}

#[derive(Clone)]
pub(crate) struct NativeMediaProfile {
    transport: Transport,
    pub(crate) rtp: Rtp,
    pub(crate) codecs: Vec<Codec>,
    pub(crate) webrtc: bool,
    pub(crate) dtmf: Dtmf,
}

impl NativeMediaProfile {
    pub(crate) fn capture(
        config: &Config,
        endpoint: &str,
        bindings: &BoundNativeProfiles,
    ) -> Result<Self, ControlError> {
        let mut media = Self::requested(config, endpoint)?;
        media.transport.bind = bindings.find(&media.transport)?.address;
        Ok(media)
    }

    fn requested(config: &Config, endpoint: &str) -> Result<Self, ControlError> {
        let endpoint = config
            .endpoints
            .iter()
            .find(|candidate| candidate.name == endpoint)
            .ok_or(ControlError::NotFound("Endpoint not found"))?;
        let transport = config
            .transports
            .iter()
            .find(|transport| transport.name == endpoint.transport)
            .ok_or(ControlError::NotFound("Transport not found"))?;
        Ok(Self {
            transport: transport.clone(),
            rtp: config.rtp.clone(),
            codecs: endpoint.codecs.clone(),
            webrtc: endpoint.webrtc,
            dtmf: endpoint.dtmf,
        })
    }

    pub(crate) fn bind_ip(&self) -> std::net::IpAddr {
        self.transport.bind.ip()
    }

    pub(crate) fn advertised(&self, local: SocketAddr) -> SocketAddr {
        SocketAddr::new(
            self.transport.external_media_address.unwrap_or(local.ip()),
            local.port(),
        )
    }
}

pub(crate) struct BoundNativeCall {
    pub(crate) endpoint: String,
    pub(crate) sip: OutboundProfile,
    pub(crate) media: NativeMediaProfile,
    pub(crate) binding: NativeTransportBinding,
}

impl NativeCallProfile {
    fn capture(
        config: &Config,
        bindings: Option<&BoundNativeProfiles>,
        endpoint: &str,
    ) -> Result<Self, ControlError> {
        let media = NativeMediaProfile::requested(config, endpoint)?;
        let listener = match bindings {
            Some(bindings) => {
                NativeListenerProfile::Bound(bindings.find(&media.transport)?.clone())
            }
            None => NativeListenerProfile::Requested(media.transport.clone()),
        };
        Ok(Self {
            endpoint: endpoint.into(),
            sip: OutboundProfile::capture(config, endpoint)
                .map_err(|error| ControlError::Adapter(error.to_string()))?,
            media,
            listener,
        })
    }

    pub(crate) fn bind(
        self,
        bindings: &BoundNativeProfiles,
    ) -> Result<BoundNativeCall, ControlError> {
        let Self {
            endpoint,
            sip,
            mut media,
            listener,
        } = self;
        let (sip, binding) = match listener {
            NativeListenerProfile::Requested(requested) => {
                let binding = bindings.find(&requested)?.clone();
                let sip = sip
                    .bind_address(requested.bind, binding.address)
                    .map_err(|error| ControlError::Adapter(error.to_string()))?;
                (sip, binding)
            }
            NativeListenerProfile::Bound(binding) => {
                if !bindings.find(&media.transport)?.same(&binding) {
                    return Err(ControlError::Conflict("Starting signaling profile changed"));
                }
                (sip, binding)
            }
        };
        media.transport.bind = binding.address;
        Ok(BoundNativeCall {
            endpoint,
            sip,
            media,
            binding,
        })
    }
}
