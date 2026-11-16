use crate::{Engine, Output, SipEvent};
use std::net::SocketAddr;

impl Engine {
    pub fn disconnected(&mut self, peer: SocketAddr) -> Output {
        let mut events = Vec::new();
        self.refers.disconnected(peer, &mut events);
        let before = self.contact_snapshot();
        self.contacts.disconnected(peer);
        self.contact_changes(&before, &mut events);
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
