use super::{verify_with, VerificationPort};
use xtask::release_check::RequiredGate;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Event {
    Read(RequiredGate),
    Fresh(RequiredGate),
    Complete,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Stopped(Event);

struct Model<'a> {
    events: &'a mut Vec<Event>,
    failure: Option<Event>,
}

impl Model<'_> {
    fn record(&mut self, event: Event) -> Result<(), Stopped> {
        self.events.push(event);
        if self.failure == Some(event) {
            Err(Stopped(event))
        } else {
            Ok(())
        }
    }
}

impl VerificationPort for Model<'_> {
    type Error = Stopped;

    fn read(&mut self, gate: RequiredGate) -> Result<(), Self::Error> {
        self.record(Event::Read(gate))
    }

    fn fresh(&mut self, gate: RequiredGate) -> Result<(), Self::Error> {
        self.record(Event::Fresh(gate))
    }

    fn complete(mut self) -> Result<(), Self::Error> {
        self.record(Event::Complete)
    }
}

fn count(events: &[Event], expected: Event) -> usize {
    events.iter().filter(|event| **event == expected).count()
}

#[test]
fn verification_reads_every_retained_gate_before_one_fast_and_one_security_check() {
    let mut events = Vec::new();
    let result = verify_with(Model {
        events: &mut events,
        failure: None,
    });
    assert!(result.is_ok());
    assert_eq!(RequiredGate::ALL.len(), 17);
    assert_eq!(events.len(), RequiredGate::ALL.len() + 1);
    assert!(events[..15]
        .iter()
        .all(|event| matches!(event, Event::Read(_))));
    assert_eq!(events.get(15), Some(&Event::Fresh(RequiredGate::Fast)));
    assert_eq!(events.get(16), Some(&Event::Fresh(RequiredGate::Security)));
    assert_eq!(events.last(), Some(&Event::Complete));
    for gate in RequiredGate::ALL {
        if matches!(gate, RequiredGate::Fast | RequiredGate::Security) {
            assert_eq!(count(&events, Event::Read(gate)), 0);
            assert_eq!(count(&events, Event::Fresh(gate)), 1);
        } else {
            assert_eq!(count(&events, Event::Read(gate)), 1);
            assert_eq!(count(&events, Event::Fresh(gate)), 0);
        }
    }
}

#[test]
fn each_retained_reader_failure_stops_before_fresh_checks_or_completion() {
    for gate in RequiredGate::ALL
        .into_iter()
        .filter(|gate| !matches!(gate, RequiredGate::Fast | RequiredGate::Security))
    {
        let mut events = Vec::new();
        let result = verify_with(Model {
            events: &mut events,
            failure: Some(Event::Read(gate)),
        });
        assert_eq!(result, Err(Stopped(Event::Read(gate))));
        assert_eq!(events.last(), Some(&Event::Read(gate)));
        assert!(events.iter().all(|event| matches!(event, Event::Read(_))));
        for required in RequiredGate::ALL {
            assert!(count(&events, Event::Read(required)) <= 1);
        }
    }
}

#[test]
fn fresh_or_completion_failures_are_not_retried_or_promoted_to_completion() {
    for (failure, fast, security, complete) in [
        (Event::Fresh(RequiredGate::Fast), 1, 0, 0),
        (Event::Fresh(RequiredGate::Security), 1, 1, 0),
        (Event::Complete, 1, 1, 1),
    ] {
        let mut events = Vec::new();
        let result = verify_with(Model {
            events: &mut events,
            failure: Some(failure),
        });
        assert_eq!(result, Err(Stopped(failure)));
        assert_eq!(events.last(), Some(&failure));
        assert_eq!(count(&events, Event::Fresh(RequiredGate::Fast)), fast);
        assert_eq!(
            count(&events, Event::Fresh(RequiredGate::Security)),
            security
        );
        assert_eq!(count(&events, Event::Complete), complete);
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, Event::Read(_)))
                .count(),
            15
        );
    }
}
