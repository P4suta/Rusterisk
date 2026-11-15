use rusterisk_core::{ResourceKind, ResourceSource, ResourceSubscriptions, SubscriptionError};

#[test]
fn interest_overflow_is_an_error_and_every_successful_interest_requires_release(
) -> Result<(), SubscriptionError> {
    let source = ResourceSource::Specific {
        kind: ResourceKind::Endpoint,
        generation: 8,
    };
    let mut interests = ResourceSubscriptions::<1>::new();
    for _ in 0..u16::MAX {
        interests.subscribe(source)?;
    }
    let full = interests;
    assert_eq!(
        interests.subscribe(source),
        Err(SubscriptionError::Capacity)
    );
    assert_eq!(interests, full);
    for _ in 1..u16::MAX {
        interests.unsubscribe(source)?;
        assert!(interests.interested_in(source));
    }
    interests.unsubscribe(source)?;
    assert!(!interests.interested_in(source));
    Ok(())
}

#[test]
fn interests_count_specific_sources_and_expiration_does_not_follow_identifier_reuse(
) -> Result<(), SubscriptionError> {
    let first = ResourceSource::Specific {
        kind: ResourceKind::Channel,
        generation: 1,
    };
    let reused = ResourceSource::Specific {
        kind: ResourceKind::Channel,
        generation: 2,
    };
    let mut interests = ResourceSubscriptions::<2>::new();
    interests.subscribe(first)?;
    interests.subscribe(first)?;
    interests.unsubscribe(first)?;
    assert!(interests.interested_in(first));
    interests.expire(first);
    assert!(!interests.interested_in(first));
    assert!(!interests.interested_in(reused));
    assert_eq!(
        interests.unsubscribe(first),
        Err(SubscriptionError::Missing)
    );
    interests.subscribe(reused)?;
    assert!(!interests.interested_in(first));
    Ok(())
}

#[test]
fn wildcard_interests_are_idempotent_and_capacity_failures_preserve_existing_sources(
) -> Result<(), SubscriptionError> {
    let channel = ResourceSource::Specific {
        kind: ResourceKind::Channel,
        generation: 1,
    };
    let bridge = ResourceSource::Specific {
        kind: ResourceKind::Bridge,
        generation: 1,
    };
    let all = ResourceSource::All(ResourceKind::Channel);
    let mut interests = ResourceSubscriptions::<2>::new();
    interests.subscribe(channel)?;
    interests.subscribe(all)?;
    interests.subscribe(all)?;
    assert_eq!(
        interests.subscribe(bridge),
        Err(SubscriptionError::Capacity)
    );
    interests.expire(channel);
    assert!(interests.interested_in(channel));
    interests.unsubscribe(all)?;
    assert!(!interests.interested_in(channel));
    assert!(!interests.interested_in(bridge));
    assert_eq!(interests.unsubscribe(all), Err(SubscriptionError::Missing));
    interests.subscribe(bridge)?;
    assert!(interests.interested_in(bridge));
    Ok(())
}
