use crate::ControlError;

pub(super) fn combine(
    primary: Result<(), ControlError>,
    cleanup: Result<(), ControlError>,
) -> Result<(), ControlError> {
    match (primary, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(primary), Err(cleanup)) => Err(ControlError::Adapter(format!("{primary}; {cleanup}"))),
    }
}

pub(super) fn each<T>(
    resources: impl IntoIterator<Item = T>,
    mut retire: impl FnMut(T) -> Result<(), ControlError>,
) -> Result<(), ControlError> {
    let mut result = Ok(());
    for resource in resources {
        result = combine(result, retire(resource));
    }
    result
}

#[cfg(test)]
mod tests {
    use super::{combine, each};
    use crate::ControlError;

    #[test]
    fn retirement_attempts_every_resource_and_keeps_both_failures() {
        let mut retired = Vec::new();
        let result = each([1, 2, 3], |resource| {
            retired.push(resource);
            match resource {
                1 => Err(ControlError::Invalid("first retire failed")),
                3 => Err(ControlError::Invalid("last retire failed")),
                _ => Ok(()),
            }
        });
        assert_eq!(retired, [1, 2, 3]);
        let error = result.err().map(|error| error.to_string());
        assert!(error
            .as_ref()
            .is_some_and(|error| error.contains("first retire failed")
                && error.contains("last retire failed")));
    }

    #[test]
    fn primary_close_and_rebind_failures_survive_aggregation() {
        let result = combine(
            combine(
                Err(ControlError::Invalid("primary exchange failed")),
                Err(ControlError::Invalid("network close failed")),
            ),
            Err(ControlError::Invalid("listener rebind failed")),
        );
        let error = result.err().map(|error| error.to_string());
        assert!(error
            .as_ref()
            .is_some_and(|error| error.contains("primary exchange failed")
                && error.contains("network close failed")
                && error.contains("listener rebind failed")));
    }

    #[test]
    fn successful_cleanup_keeps_the_original_error_and_success_stays_successful() {
        assert!(combine(Ok(()), Ok(())).is_ok());
        assert!(matches!(
            combine(Err(ControlError::Capacity), Ok(())),
            Err(ControlError::Capacity)
        ));
        assert!(matches!(
            combine(Ok(()), Err(ControlError::Unsupported)),
            Err(ControlError::Unsupported)
        ));
    }
}
