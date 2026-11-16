use crate::ControlError;
use rusterisk_config::Registration;
use std::num::NonZeroU32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Decision {
    Stop,
    Wait { at: u64, retries: u32 },
}

pub(super) fn decide(
    settings: &Registration,
    retries: u32,
    status: u16,
    instruction: Option<NonZeroU32>,
    now: u64,
) -> Result<Decision, ControlError> {
    if !(300..=699).contains(&status) || status == 423 {
        return Err(ControlError::Invalid(
            "Expected a completed registration failure",
        ));
    }
    if let Some(interval) = instruction {
        return wait(retries, interval, now);
    }
    let temporary = matches!(status, 408 | 500 | 502..=504 | 600..=699)
        || matches!(status, 401 | 407) && !settings.auth_rejection_permanent;
    let interval = if temporary && settings.retry_interval > 0 {
        settings.retry_interval
    } else if status == 403 && settings.forbidden_retry_interval > 0 {
        settings.forbidden_retry_interval
    } else {
        settings.fatal_retry_interval
    };
    let Some(interval) = NonZeroU32::new(interval) else {
        return Ok(Decision::Stop);
    };
    if retries >= settings.max_retries {
        return Ok(Decision::Stop);
    }
    wait(
        retries.checked_add(1).ok_or(ControlError::Capacity)?,
        interval,
        now,
    )
}

fn wait(retries: u32, interval: NonZeroU32, now: u64) -> Result<Decision, ControlError> {
    let at = now
        .checked_add(u64::from(interval.get()) * 1000)
        .ok_or(ControlError::Capacity)?;
    Ok(Decision::Wait { at, retries })
}

#[cfg(test)]
mod tests {
    use super::{decide, Decision};
    use crate::{signaling::registrations::tests::config, ControlError};
    use rusterisk_config::Registration;
    use std::num::NonZeroU32;

    fn settings() -> Result<Registration, ControlError> {
        let mut config = config(&["127.0.0.2:5070"], 0)?;
        let mut settings = config.registrations.pop().ok_or(ControlError::Capacity)?;
        settings.retry_interval = 7;
        settings.max_retries = 2;
        Ok(settings)
    }

    #[test]
    fn positive_instructions_preserve_counters_after_exhaustion() -> Result<(), ControlError> {
        let mut settings = settings()?;
        settings.max_retries = 0;
        settings.retry_interval = 0;
        let hint = NonZeroU32::new(3).ok_or(ControlError::Capacity)?;
        for status in [400, 401, 403, 404, 408, 500, 501, 503, 600, 699] {
            for retries in [0, 1, u32::MAX] {
                assert_eq!(
                    decide(&settings, retries, status, Some(hint), 100)?,
                    Decision::Wait { at: 3100, retries }
                );
            }
        }
        Ok(())
    }

    #[test]
    fn temporary_statuses_use_the_configured_interval_and_budget() -> Result<(), ControlError> {
        let settings = settings()?;
        for status in [408, 500, 502, 503, 504, 600, 603, 699] {
            assert_eq!(
                decide(&settings, 0, status, None, 100)?,
                Decision::Wait {
                    at: 7100,
                    retries: 1
                }
            );
            assert_eq!(decide(&settings, 2, status, None, 100)?, Decision::Stop);
            assert_eq!(
                decide(&settings, u32::MAX, status, None, 100)?,
                Decision::Stop
            );
        }
        Ok(())
    }

    #[test]
    fn authentication_rejection_defaults_to_permanent_without_a_positive_instruction(
    ) -> Result<(), ControlError> {
        let mut settings = settings()?;
        for status in [401, 407] {
            settings.auth_rejection_permanent = true;
            assert_eq!(decide(&settings, 0, status, None, 100)?, Decision::Stop);
            settings.auth_rejection_permanent = false;
            assert_eq!(
                decide(&settings, 0, status, None, 100)?,
                Decision::Wait {
                    at: 7100,
                    retries: 1
                }
            );
        }
        Ok(())
    }

    #[test]
    fn forbidden_and_fatal_intervals_follow_reference_precedence() -> Result<(), ControlError> {
        let mut settings = settings()?;
        settings.forbidden_retry_interval = 5;
        settings.fatal_retry_interval = 9;
        assert_eq!(
            decide(&settings, 0, 403, None, 100)?,
            Decision::Wait {
                at: 5100,
                retries: 1
            }
        );
        assert_eq!(
            decide(&settings, 0, 500, None, 100)?,
            Decision::Wait {
                at: 7100,
                retries: 1
            }
        );
        for status in [400, 401, 404, 421, 501, 505] {
            assert_eq!(
                decide(&settings, 0, status, None, 100)?,
                Decision::Wait {
                    at: 9100,
                    retries: 1
                }
            );
        }
        assert_eq!(decide(&settings, 2, 500, None, 100)?, Decision::Stop);
        settings.retry_interval = 0;
        assert_eq!(
            decide(&settings, 0, 500, None, 100)?,
            Decision::Wait {
                at: 9100,
                retries: 1
            }
        );
        settings.forbidden_retry_interval = 0;
        assert_eq!(
            decide(&settings, 0, 403, None, 100)?,
            Decision::Wait {
                at: 9100,
                retries: 1
            }
        );
        settings.fatal_retry_interval = 0;
        assert_eq!(decide(&settings, 0, 500, None, 100)?, Decision::Stop);
        Ok(())
    }

    #[test]
    fn counter_and_clock_boundaries_never_wrap() -> Result<(), ControlError> {
        let mut settings = settings()?;
        settings.max_retries = u32::MAX;
        settings.retry_interval = u32::MAX;
        let interval = u64::from(u32::MAX) * 1000;
        assert_eq!(
            decide(&settings, u32::MAX - 1, 500, None, 0)?,
            Decision::Wait {
                at: interval,
                retries: u32::MAX
            }
        );
        assert_eq!(decide(&settings, u32::MAX, 500, None, 0)?, Decision::Stop);
        assert!(matches!(
            decide(&settings, 0, 500, None, u64::MAX - interval + 1),
            Err(ControlError::Capacity)
        ));
        let hint = NonZeroU32::new(1).ok_or(ControlError::Capacity)?;
        assert!(matches!(
            decide(&settings, 0, 500, Some(hint), u64::MAX - 999),
            Err(ControlError::Capacity)
        ));
        Ok(())
    }

    #[test]
    fn successful_provisional_and_minimum_correction_responses_require_their_own_paths(
    ) -> Result<(), ControlError> {
        let settings = settings()?;
        for status in [0, 100, 199, 200, 299, 423, 700, u16::MAX] {
            assert!(decide(&settings, 0, status, None, 100).is_err());
        }
        Ok(())
    }
}
