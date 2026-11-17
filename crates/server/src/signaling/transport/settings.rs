use super::{ListenerSocket, Network, Scheme, TlsClientContext, TlsContext};
use crate::ControlError;
use rusterisk_config::{Protocol, Transport, MAX_TRANSPORTS};

enum PreparedListener {
    Datagram,
    Stream,
    Encrypted {
        context: TlsContext,
        client: TlsClientContext,
    },
    WebSocket(Scheme),
}

impl PreparedListener {
    fn protocol(&self) -> Protocol {
        match self {
            Self::Datagram => Protocol::Udp,
            Self::Stream => Protocol::Tcp,
            Self::Encrypted { .. } => Protocol::Tls,
            Self::WebSocket(Scheme::Ws) => Protocol::Ws,
            Self::WebSocket(Scheme::Wss) => Protocol::Wss,
        }
    }
}

pub(in crate::signaling) struct PreparedNetworkSettings {
    listeners: Vec<PreparedListener>,
}

impl PreparedNetworkSettings {
    pub(in crate::signaling) async fn prepare(
        transports: &[Transport],
    ) -> Result<(Self, Vec<Option<TlsClientContext>>), ControlError> {
        if transports.len() > MAX_TRANSPORTS {
            return Err(ControlError::Capacity);
        }
        let mut listeners = Vec::with_capacity(transports.len());
        let mut clients = Vec::with_capacity(transports.len());
        for transport in transports {
            let listener =
                match transport.protocol {
                    Protocol::Udp => PreparedListener::Datagram,
                    Protocol::Tcp => PreparedListener::Stream,
                    Protocol::Tls => {
                        let context =
                            TlsContext::load(transport.tls.as_ref().ok_or(
                                ControlError::Invalid("Encryption configuration missing"),
                            )?)
                            .await?;
                        let client = TlsClientContext::load_sip(transport.ca_list_file.as_deref())?;
                        PreparedListener::Encrypted { context, client }
                    }
                    Protocol::Ws => PreparedListener::WebSocket(Scheme::Ws),
                    Protocol::Wss => PreparedListener::WebSocket(Scheme::Wss),
                };
            clients.push(match &listener {
                PreparedListener::Encrypted { client, .. } => Some(client.clone()),
                PreparedListener::Datagram
                | PreparedListener::Stream
                | PreparedListener::WebSocket(_) => None,
            });
            listeners.push(listener);
        }
        Ok((Self { listeners }, clients))
    }

    pub(in crate::signaling) fn validate(&self, network: &Network) -> Result<(), ControlError> {
        if self.listeners.len() != network.listeners.len() {
            return Err(ControlError::Conflict(
                "Signaling listener replacement requires restart",
            ));
        }
        for (prepared, listener) in self.listeners.iter().zip(&network.listeners) {
            let protocol = match listener {
                ListenerSocket::Datagram(_) => Protocol::Udp,
                ListenerSocket::Stream(_) => Protocol::Tcp,
                ListenerSocket::Encrypted { .. } => Protocol::Tls,
                ListenerSocket::WebSocket {
                    scheme: Scheme::Ws, ..
                } => Protocol::Ws,
                ListenerSocket::WebSocket {
                    scheme: Scheme::Wss,
                    ..
                } => Protocol::Wss,
            };
            if prepared.protocol() != protocol {
                return Err(ControlError::Conflict(
                    "Signaling listener replacement requires restart",
                ));
            }
        }
        Ok(())
    }
}

impl Network {
    pub(in crate::signaling) fn publish_settings(
        &mut self,
        prepared: PreparedNetworkSettings,
    ) -> Result<(), ControlError> {
        prepared.validate(self)?;
        for (listener, prepared) in self.listeners.iter_mut().zip(prepared.listeners) {
            if let PreparedListener::Encrypted { context, client } = prepared {
                if let ListenerSocket::Encrypted {
                    context: current,
                    client: current_client,
                    ..
                } = listener
                {
                    *current = context;
                    *current_client = client;
                } else {
                    return Err(ControlError::Conflict(
                        "Signaling listener changed during publication",
                    ));
                }
            }
        }
        Ok(())
    }
}
