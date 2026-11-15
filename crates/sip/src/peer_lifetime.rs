use crate::{Engine, Output, SipEvent};
use rusterisk_core::ContactPresence;
use std::net::SocketAddr;

impl Engine {
    pub fn disconnected(&mut self, peer: SocketAddr) -> Output {
        let mut events = Vec::new();
        self.contacts.retain(|endpoint, contact| {
            if contact.peer != peer {
                return true;
            }
            events.push(SipEvent::EndpointContact {
                endpoint: endpoint.clone(),
                generation: self.configuration_generation,
                presence: ContactPresence::Absent,
            });
            false
        });
        self.cache.retain(|_, cached| cached.peer != peer);
        self.nonces.retain(|_, nonce| nonce.peer != peer);
        self.pending.retain(|_, invite| {
            if invite.peer != peer {
                return true;
            }
            events.push(SipEvent::IncomingEnded {
                id: invite.id.clone(),
                cause: 41,
            });
            false
        });
        self.incoming_disconnected(peer, &mut events);
        self.outgoing_disconnected(peer, &mut events);
        self.transactions.disconnected(peer);
        Output {
            datagrams: Vec::new(),
            events,
        }
    }
}
