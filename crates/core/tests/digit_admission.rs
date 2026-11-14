use rusterisk_core::{DtmfDigit, DtmfError, DtmfOutbox, DtmfTiming};

#[test]
fn digit_commands_reserve_bounded_slots_and_only_matching_completions_release_them(
) -> Result<(), DtmfError> {
    let timing = DtmfTiming::new(0, 100, 100, 0)?;
    let mut outbox = DtmfOutbox::new(1 << DtmfDigit::Hash.event())?;
    assert_eq!(
        outbox.admit(&[DtmfDigit::One], timing),
        Err(DtmfError::Unsupported)
    );
    assert_eq!(outbox.admit(&[], timing), Err(DtmfError::Empty));
    let mut tokens = Vec::new();
    for _ in 0..8 {
        tokens.push(outbox.admit(&[DtmfDigit::Hash], timing)?.token);
    }
    assert_eq!(
        outbox.admit(&[DtmfDigit::Hash], timing),
        Err(DtmfError::Capacity)
    );
    assert!(outbox.finish(tokens[3]));
    assert!(!outbox.finish(tokens[3]));
    let mut command = outbox.admit(&[DtmfDigit::Hash], timing)?;
    assert!(!tokens.contains(&command.token));
    assert_eq!(
        command.sequence.advance(0)?.ok_or(DtmfError::Empty)?.digit,
        DtmfDigit::Hash
    );
    assert_eq!(
        outbox.admit(&[DtmfDigit::Hash], timing),
        Err(DtmfError::Capacity)
    );
    outbox.cancel();
    for token in tokens {
        assert!(!outbox.finish(token));
    }
    assert!(!outbox.finish(command.token));
    assert_eq!(
        outbox.admit(&[DtmfDigit::Hash], timing),
        Err(DtmfError::Ended)
    );
    assert!(DtmfOutbox::new(0).is_err());
    Ok(())
}
