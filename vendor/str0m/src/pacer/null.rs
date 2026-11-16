use std::collections::HashMap;
use std::time::Instant;

use super::Pacer;
use super::PaddingRequest;
use super::QueueId;
use super::QueueState;
use crate::Reason;
use crate::pacer::PacerReason;
use crate::rtp_::{Bitrate, DataSize, TwccClusterId};

/// A null pacer that doesn't pace.
#[derive(Debug, Default)]
pub struct NullPacer {
    last_sends: HashMap<QueueId, Instant>,
    /// Snapshots from the last `handle_timeout`. `poll_queue` counts off
    /// each packet it hands out.
    queue_states: Vec<QueueState>,
    /// Set when `poll_queue` hands out a queue that `register_send` never confirms:
    /// the stream had nothing to send after all, so the snapshot is stale.
    unsent_since: Option<Instant>,
}

impl Pacer for NullPacer {
    fn set_pacing_rate(&mut self, _padding_bitrate: Bitrate) {
        // We don't care
    }

    fn set_padding_rate(&mut self, _padding_bitrate: Bitrate) {
        // We don't care
    }
    fn poll_timeout(&self) -> (Option<Instant>, Reason) {
        // Only a poll that sent nothing needs a fresh snapshot: other queued packets go
        // out on the next poll. New media requests a timeout, and receiving a NACK
        // refreshes the snapshot through Rtc::handle_input before polling resumes.
        (self.unsent_since, Reason::Pacer(PacerReason::Handle))
    }

    fn handle_timeout(
        &mut self,
        _now: Instant,
        iter: impl Iterator<Item = QueueState>,
    ) -> Option<PaddingRequest> {
        self.unsent_since = None;
        self.queue_states.clear();
        self.queue_states.extend(iter);
        self.last_sends
            .retain(|id, _| self.queue_states.iter().any(|queue| queue.id == *id));

        None
    }

    fn poll_queue(&mut self) -> Option<(QueueId, Option<TwccClusterId>)> {
        // Pick a queue using round robin, prioritize the least recently sent on queue.
        let queue = self
            .queue_states
            .iter_mut()
            .filter(|q| q.snapshot.packet_count > 0)
            .min_by_key(|q| self.last_sends.get(&q.id))?;

        queue.snapshot.packet_count -= 1;
        self.unsent_since = Some(queue.snapshot.created_at);

        Some((queue.id, None))
    }

    fn register_send(&mut self, now: Instant, _packet_size: DataSize, from: QueueId) {
        self.unsent_since = None;
        let e = self.last_sends.entry(from).or_insert(now);
        *e = now;
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::pacer::QueueSnapshot;
    use crate::rtp_::MidRid;

    fn queue(mid: &str, source: u32, packet_count: u32, now: Instant) -> QueueState {
        QueueState {
            id: QueueId {
                midrid: MidRid(mid.into(), None),
                ssrc: source.into(),
            },
            unpaced: true,
            use_for_padding: false,
            snapshot: QueueSnapshot {
                created_at: now,
                packet_count,
                ..Default::default()
            },
        }
    }

    #[test]
    fn drains_every_queue_without_another_timeout() {
        let now = Instant::now();
        let mut pacer = NullPacer::default();
        let queues = [
            queue("a", 1, 2, now),
            queue("b", 2, 1, now),
            queue("c", 3, 0, now),
        ];
        pacer.handle_timeout(now, queues.into_iter());

        let mut sent = vec![];
        for _ in 0..3 {
            let (midrid, _) = pacer.poll_queue().expect("queued packet");
            pacer.register_send(now, DataSize::bytes(100), midrid);
            assert_eq!(pacer.poll_timeout().0, None, "no timeout between sends");
            sent.push(midrid.midrid.mid().to_string());
        }
        sent.sort();
        assert_eq!(sent, ["a", "a", "b"]);
        assert_eq!(pacer.poll_queue(), None);
        assert_eq!(pacer.poll_timeout().0, None, "no timeout after draining");
    }

    #[test]
    fn unsent_poll_asks_for_a_fresh_snapshot() {
        let now = Instant::now();
        let mut pacer = NullPacer::default();
        pacer.handle_timeout(
            now,
            [queue("a", 1, 1, now), queue("b", 2, 1, now)].into_iter(),
        );

        assert!(pacer.poll_queue().is_some());
        assert_eq!(pacer.poll_timeout().0, Some(now), "stream sent nothing");

        pacer.handle_timeout(now, [queue("b", 2, 1, now)].into_iter());
        assert_eq!(pacer.poll_timeout().0, None);
        let (midrid, _) = pacer.poll_queue().expect("remaining packet");
        assert_eq!(midrid, queue("b", 2, 1, now).id);
        pacer.register_send(now, DataSize::bytes(100), midrid);
        assert_eq!(pacer.poll_queue(), None);
        assert_eq!(pacer.poll_timeout().0, None);
    }

    #[test]
    fn round_robin_by_least_recently_sent() {
        let now = Instant::now();
        let mut pacer = NullPacer::default();
        pacer.register_send(now, DataSize::bytes(100), queue("a", 1, 1, now).id);
        let later = now + std::time::Duration::from_millis(1);
        pacer.register_send(later, DataSize::bytes(100), queue("b", 2, 1, now).id);
        pacer.handle_timeout(
            later,
            [queue("a", 1, 1, later), queue("b", 2, 1, later)].into_iter(),
        );

        assert_eq!(pacer.poll_queue(), Some((queue("a", 1, 1, later).id, None)));
    }

    #[test]
    fn source_histories_are_distinct_and_removed_with_their_queues() {
        let now = Instant::now();
        let first = queue("a", 7, 1, now);
        let second = queue("a", 8, 1, now);
        let mut pacer = NullPacer::default();
        pacer.handle_timeout(now, [first, second].into_iter());
        for queue in [first, second] {
            assert_eq!(pacer.poll_queue(), Some((queue.id, None)));
            pacer.register_send(now, DataSize::bytes(100), queue.id);
        }
        assert_eq!(pacer.last_sends.len(), 2);
        pacer.handle_timeout(now, std::iter::empty());
        assert!(pacer.last_sends.is_empty());
    }
}
