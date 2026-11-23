use rusterisk_core::Wake;
use rusterisk_sip::{Engine, Output, Retained, SipError, SipEvent};

pub struct Twins {
    dense: Engine,
    sparse: Engine,
    now: u64,
    skipped: u64,
}

fn event(event: &SipEvent) -> String {
    match event {
        SipEvent::ForkTeardownFailed { id, generation, .. } => {
            format!("fork teardown failed {id} {generation}")
        }
        SipEvent::AttendedTransferRequested { id, generation, .. } => {
            format!("attended transfer {id} {generation}")
        }
        SipEvent::BlindTransferRequested { id, generation, .. } => {
            format!("blind transfer {id} {generation}")
        }
        SipEvent::ReferSubscriptionEnded { refer } => format!("refer ended {refer:?}"),
        SipEvent::EndpointContact {
            endpoint,
            generation,
            presence,
        } => format!("contact {endpoint} {generation} {presence:?}"),
        SipEvent::PeerDirection {
            id,
            generation,
            direction,
        } => format!("direction {id} {generation} {direction:?}"),
        SipEvent::DtmfReceived {
            id,
            generation,
            digit,
            duration_ms,
        } => format!("digit {id} {generation} {digit:?} {duration_ms}"),
        SipEvent::Ringing { id, generation } => format!("ringing {id} {generation}"),
        SipEvent::Answered { id, generation, .. } => format!("answered {id} {generation}"),
        SipEvent::Ended {
            id,
            generation,
            cause,
        } => format!("ended {id} {generation} {cause}"),
        SipEvent::Incoming {
            id,
            endpoint,
            identity,
            caller,
            context,
            extension,
            ..
        } => format!("incoming {id} {endpoint} {identity} {caller} {context} {extension}"),
        SipEvent::IncomingEnded { id, cause } => format!("incoming ended {id} {cause}"),
    }
}

fn record(output: &Output) -> Vec<String> {
    output
        .datagrams
        .iter()
        .map(|datagram| {
            format!(
                "{} {:?} {:?} {}",
                datagram.peer,
                datagram.origin(),
                datagram.flow(),
                String::from_utf8_lossy(&datagram.bytes)
            )
        })
        .chain(output.events.iter().map(event))
        .collect()
}

fn outcome(result: &Result<Output, SipError>) -> Result<Vec<String>, String> {
    result.as_ref().map(record).map_err(ToString::to_string)
}

impl Twins {
    pub fn new(build: impl Fn() -> Result<Engine, SipError>) -> Result<Self, SipError> {
        Ok(Self {
            dense: build()?,
            sparse: build()?,
            now: 0,
            skipped: 0,
        })
    }

    pub const fn skipped(&self) -> u64 {
        self.skipped
    }

    pub fn retained(&self) -> Retained {
        self.dense.retained()
    }

    fn compare(&self) {
        assert_eq!(
            self.dense.retained(),
            self.sparse.retained(),
            "retained state diverged at {} ms",
            self.now
        );
        assert_eq!(
            self.dense.wake(),
            self.sparse.wake(),
            "reported wakes diverged at {} ms",
            self.now
        );
    }

    pub fn apply(
        &mut self,
        input: impl Fn(&mut Engine, u64) -> Result<Output, SipError>,
    ) -> Result<Output, SipError> {
        let dense = input(&mut self.dense, self.now);
        let sparse = input(&mut self.sparse, self.now);
        assert_eq!(
            outcome(&dense),
            outcome(&sparse),
            "an input diverged at {} ms",
            self.now
        );
        self.compare();
        dense
    }

    pub fn advance(&mut self, until: u64) -> Result<Output, SipError> {
        let mut collected = Output {
            datagrams: Vec::new(),
            events: Vec::new(),
        };
        while self.now < until {
            self.now += 1;
            let dense = self.dense.tick(self.now)?;
            let due = match self.sparse.wake() {
                Wake::Now => true,
                Wake::At(at) => at <= self.now,
                Wake::Idle => false,
            };
            if due {
                let sparse = self.sparse.tick(self.now)?;
                assert_eq!(
                    record(&dense),
                    record(&sparse),
                    "a tick diverged at {} ms",
                    self.now
                );
            } else {
                self.skipped += 1;
                assert_eq!(
                    record(&dense),
                    Vec::<String>::new(),
                    "the engine acted at {} ms before its reported wake {:?}",
                    self.now,
                    self.sparse.wake()
                );
            }
            self.compare();
            collected.datagrams.extend(dense.datagrams);
            collected.events.extend(dense.events);
        }
        Ok(collected)
    }
}
