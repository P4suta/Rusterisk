use crate::{Disconnection, Engine, Output, SipEvent, StreamFlowId};
use std::net::SocketAddr;

impl Engine {
    pub fn disconnected(&mut self, peer: SocketAddr) -> Output {
        self.disconnect(Disconnection::Legacy(peer))
    }

    pub fn disconnected_flow(&mut self, flow: StreamFlowId) -> Output {
        self.disconnect(Disconnection::Stream(flow))
    }

    fn disconnect(&mut self, closed: Disconnection) -> Output {
        let mut events = Vec::new();
        self.refers.disconnected(closed, &mut events);
        let before = self.contact_snapshot();
        self.contacts.disconnected(closed);
        self.contact_changes(&before, &mut events);
        self.cache.retain(|_, cached| !closed.matches(cached.route));
        self.nonces.retain(|_, nonce| !closed.matches(nonce.route));
        self.pending.retain(|_, invite| {
            if !closed.matches(invite.route()) {
                return true;
            }
            events.push(SipEvent::IncomingEnded {
                id: invite.id.clone(),
                cause: 41,
            });
            false
        });
        self.incoming_disconnected(closed, &mut events);
        self.outgoing_disconnected(closed, &mut events);
        self.transactions.disconnected(closed);
        Output {
            datagrams: Vec::new(),
            events,
        }
    }
}
