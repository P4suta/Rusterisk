use rusterisk_core::{MediaTaskError, MediaTaskKind, MediaTaskState, MediaTasks};

#[test]
fn playback_tasks_start_in_order_and_pause_without_releasing_their_slot(
) -> Result<(), MediaTaskError> {
    let mut tasks = MediaTasks::new(12);
    let first = tasks.reserve(MediaTaskKind::Playback)?;
    let second = tasks.reserve(MediaTaskKind::Playback)?;
    assert_eq!(tasks.next_playback(), Some(first));
    assert_eq!(tasks.start(second), Err(MediaTaskError::Order));
    tasks.start(first)?;
    assert_eq!(tasks.next_playback(), None);
    tasks.pause(first)?;
    assert_eq!(tasks.state(first), Some(MediaTaskState::Paused));
    assert_eq!(tasks.next_playback(), None);
    tasks.resume(first)?;
    assert_eq!(tasks.state(first), Some(MediaTaskState::Active));
    assert!(tasks.finish(first));
    assert!(!tasks.finish(first));
    assert_eq!(tasks.next_playback(), Some(second));
    tasks.start(second)?;
    Ok(())
}

#[test]
fn cancellation_releases_every_owned_task_once_and_rejects_late_work() -> Result<(), MediaTaskError>
{
    for count in 1..=8 {
        let mut tasks = MediaTasks::new(7);
        let mut issued = Vec::new();
        for _ in 0..count {
            issued.push(tasks.reserve(MediaTaskKind::Playback)?);
        }
        let released = tasks.cancel();
        assert_eq!(released.into_iter().flatten().collect::<Vec<_>>(), issued);
        assert!(tasks.cancel().iter().all(Option::is_none));
        for token in issued {
            assert!(!tasks.finish(token));
            assert_eq!(tasks.start(token), Err(MediaTaskError::Ended));
        }
        assert_eq!(
            tasks.reserve(MediaTaskKind::Recording),
            Err(MediaTaskError::Ended)
        );
    }
    Ok(())
}

#[test]
fn task_capacity_and_generation_prevent_stale_completions_from_releasing_new_work(
) -> Result<(), MediaTaskError> {
    let mut tasks = MediaTasks::new(8);
    let first = tasks.reserve(MediaTaskKind::Recording)?;
    assert_eq!(
        tasks.reserve(MediaTaskKind::Recording),
        Err(MediaTaskError::Recording)
    );
    for _ in 0..7 {
        tasks.reserve(MediaTaskKind::Playback)?;
    }
    assert_eq!(
        tasks.reserve(MediaTaskKind::Playback),
        Err(MediaTaskError::Capacity)
    );
    assert!(tasks.finish(first));
    let fresh = tasks.reserve(MediaTaskKind::Recording)?;
    assert_ne!(first, fresh);
    assert!(!tasks.finish(first));
    let mut another = MediaTasks::new(9);
    let other = another.reserve(MediaTaskKind::Recording)?;
    assert!(!tasks.finish(other));
    assert_eq!(tasks.start(other), Err(MediaTaskError::Unknown));
    assert_eq!(tasks.state(fresh), Some(MediaTaskState::Queued));
    tasks.start(fresh)?;
    assert_eq!(tasks.state(fresh), Some(MediaTaskState::Active));
    Ok(())
}
