use rusterisk_api::ChannelState;
use rusterisk_server::{ControlError, Effect};
#[path = "attended_control/support.rs"]
mod support;
use support::Fixture;
#[path = "attended_control/handoff.rs"]
mod handoff;

#[test]
fn two_authenticated_dial_pairs_have_four_live_generations_and_distinct_bridges(
) -> Result<(), ControlError> {
    let mut fixture = Fixture::new()?;
    let (refer, replacement) = fixture.request()?;
    assert_eq!(replacement.id(), fixture.consultation.source.id.as_str());
    assert_eq!(
        replacement.generation(),
        fixture.consultation.source.generation
    );
    assert_eq!(
        refer
            .authority()
            .replacement_authority(replacement.authority()),
        Ok(())
    );
    assert_ne!(fixture.source.bridge, fixture.consultation.bridge);
    let calls = [
        &fixture.source.source,
        &fixture.source.peer,
        &fixture.consultation.source,
        &fixture.consultation.peer,
    ];
    for (index, call) in calls.iter().enumerate() {
        assert_eq!(fixture.control.channel(&call.id)?.state, ChannelState::Up);
        assert_eq!(fixture.control.key(&call.id)?, **call);
        assert!(calls[..index]
            .iter()
            .all(|previous| previous.generation != call.generation));
    }
    let stopped = fixture.control.shutdown("stop")?;
    for call in calls {
        assert!(stopped
            .effects
            .iter()
            .any(|effect| matches!(effect, Effect::Release { key } if key == call)));
        assert!(fixture.control.channel(&call.id).is_err());
    }
    Ok(())
}
