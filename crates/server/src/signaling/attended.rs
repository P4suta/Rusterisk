use super::Signaling;
use rusterisk_sip::{ReferFailure, ReplacementDialog, Replaces};

fn unique_consultation<T>(
    candidates: impl IntoIterator<Item = Result<Option<T>, ReferFailure>>,
) -> Result<T, ReferFailure> {
    let mut selected = None;
    for (index, candidate) in candidates.into_iter().enumerate() {
        if index >= rusterisk_config::MAX_TRANSPORTS {
            return Err(ReferFailure::Unavailable);
        }
        let candidate = match candidate {
            Ok(None) => continue,
            Ok(Some(candidate)) => Ok(candidate),
            Err(
                failure @ (ReferFailure::NotFound
                | ReferFailure::Internal
                | ReferFailure::Unavailable
                | ReferFailure::Timeout
                | ReferFailure::Cancelled),
            ) => return Err(failure),
            Err(
                failure @ (ReferFailure::Forbidden
                | ReferFailure::Unsupported
                | ReferFailure::Busy
                | ReferFailure::Declined),
            ) => Err(failure),
        };
        if selected.replace(candidate).is_some() {
            return Err(ReferFailure::NotFound);
        }
    }
    match selected {
        Some(candidate) => candidate,
        None => Err(ReferFailure::NotFound),
    }
}

impl Signaling {
    pub(super) fn consultation(
        &self,
        selector: &Replaces,
    ) -> Result<ReplacementDialog, ReferFailure> {
        unique_consultation(
            self.listeners
                .iter()
                .map(|listener| listener.engine.replacement_dialog(selector)),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::unique_consultation;
    use rusterisk_sip::ReferFailure;

    #[test]
    fn every_listener_participates_in_unique_consultation_selection() {
        assert_eq!(unique_consultation::<u64>([]), Err(ReferFailure::NotFound));
        assert_eq!(
            unique_consultation([Ok(None), Ok(Some(7)), Ok(None)]),
            Ok(7)
        );
        assert_eq!(
            unique_consultation([Ok(Some(7)), Ok(Some(8))]),
            Err(ReferFailure::NotFound)
        );
        assert_eq!(
            unique_consultation([Ok(Some(7)), Ok(Some(7))]),
            Err(ReferFailure::NotFound)
        );
        for failure in [
            ReferFailure::Forbidden,
            ReferFailure::Unsupported,
            ReferFailure::Busy,
            ReferFailure::Declined,
        ] {
            assert_eq!(unique_consultation::<u64>([Err(failure)]), Err(failure));
            assert_eq!(
                unique_consultation([Err(failure), Ok(Some(7))]),
                Err(ReferFailure::NotFound)
            );
            assert_eq!(
                unique_consultation([Ok(Some(7)), Err(failure)]),
                Err(ReferFailure::NotFound)
            );
        }
    }

    #[test]
    fn ambiguous_or_failed_engine_queries_never_select_another_listener() {
        for failure in [
            ReferFailure::NotFound,
            ReferFailure::Internal,
            ReferFailure::Unavailable,
            ReferFailure::Timeout,
            ReferFailure::Cancelled,
        ] {
            assert_eq!(
                unique_consultation([Ok(Some(7)), Err(failure)]),
                Err(failure)
            );
            assert_eq!(
                unique_consultation([Err(failure), Ok(Some(7))]),
                Err(failure)
            );
        }
    }

    #[test]
    fn consultation_selection_keeps_the_configured_listener_limit() {
        let empty = Ok::<Option<u64>, ReferFailure>(None);
        let supported =
            std::iter::repeat_n(empty, rusterisk_config::MAX_TRANSPORTS - 1).chain([Ok(Some(7))]);
        assert_eq!(unique_consultation(supported), Ok(7));
        let excessive =
            std::iter::repeat_n(empty, rusterisk_config::MAX_TRANSPORTS).chain([Ok(Some(7))]);
        assert_eq!(
            unique_consultation(excessive),
            Err(ReferFailure::Unavailable)
        );
    }
}
