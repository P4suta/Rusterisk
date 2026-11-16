#[path = "../../../xtask/src/distribution/consumer_sources/http_contract.rs"]
mod http_contract;
mod sdk_distribution_http {
    pub(super) mod fixture;
}

use sdk_distribution_http::fixture::{run, Mode, Step};

#[tokio::test]
async fn the_http_consumer_checks_typed_responses_and_removes_its_bridge(
) -> Result<(), std::io::Error> {
    let (outcome, report) = run(Mode::Successful).await?;
    outcome.map_err(std::io::Error::other)?;
    assert!(!report.exists);
    assert_eq!(
        report.steps,
        [
            Step::Ping,
            Step::Info,
            Step::Inspect,
            Step::Create,
            Step::Inspect,
            Step::List,
            Step::Delete,
            Step::Inspect,
            Step::List,
        ]
    );
    Ok(())
}

#[tokio::test]
async fn a_preexisting_bridge_is_neither_created_nor_deleted() -> Result<(), std::io::Error> {
    let (outcome, report) = run(Mode::Preexisting).await?;
    assert!(outcome.is_err());
    assert!(report.exists);
    assert_eq!(report.steps, [Step::Ping, Step::Info, Step::Inspect]);
    Ok(())
}

#[tokio::test]
async fn a_lost_creation_response_still_cleans_up_the_reserved_bridge() -> Result<(), std::io::Error>
{
    let (outcome, report) = run(Mode::LostCreationResponse).await?;
    assert!(outcome.is_err());
    assert!(!report.exists);
    assert_eq!(
        report.steps,
        [
            Step::Ping,
            Step::Info,
            Step::Inspect,
            Step::Create,
            Step::Delete,
            Step::Inspect,
            Step::List,
        ]
    );
    Ok(())
}

#[tokio::test]
async fn primary_and_cleanup_failures_are_both_preserved_without_response_text(
) -> Result<(), std::io::Error> {
    let (outcome, report) = run(Mode::ListAndCleanupFailure).await?;
    let error = outcome
        .err()
        .ok_or_else(|| std::io::Error::other("expected HTTP failure"))?;
    let message = error.to_string();
    assert!(message.contains("list"));
    assert!(message.contains("delete"));
    assert!(!message.contains("fixture-password"));
    assert!(report.exists);
    assert_eq!(report.steps.last(), Some(&Step::Delete));
    Ok(())
}

#[tokio::test]
async fn a_wrong_server_version_fails_before_any_bridge_request() -> Result<(), std::io::Error> {
    let (outcome, report) = run(Mode::WrongVersion).await?;
    assert!(outcome.is_err());
    assert!(!report.exists);
    assert_eq!(report.steps, [Step::Ping, Step::Info]);
    Ok(())
}

#[tokio::test]
async fn rejected_and_invalid_creation_responses_still_attempt_reserved_cleanup(
) -> Result<(), std::io::Error> {
    for mode in [Mode::RejectedCreation, Mode::WrongCreatedResponse] {
        let (outcome, report) = run(mode).await?;
        assert!(outcome.is_err());
        assert!(!report.exists);
        assert_eq!(
            report.steps,
            [
                Step::Ping,
                Step::Info,
                Step::Inspect,
                Step::Create,
                Step::Delete,
                Step::Inspect,
                Step::List
            ]
        );
    }
    Ok(())
}

#[tokio::test]
async fn an_explicit_creation_conflict_does_not_delete_a_concurrent_bridge(
) -> Result<(), std::io::Error> {
    let (outcome, report) = run(Mode::CreationConflict).await?;
    assert!(outcome.is_err());
    assert!(report.exists);
    assert_eq!(
        report.steps,
        [Step::Ping, Step::Info, Step::Inspect, Step::Create]
    );
    Ok(())
}

#[tokio::test]
async fn successful_delete_responses_must_establish_actual_bridge_absence(
) -> Result<(), std::io::Error> {
    let (outcome, report) = run(Mode::DeleteIgnored).await?;
    assert!(outcome.is_err());
    assert!(report.exists);
    assert_eq!(report.steps.last(), Some(&Step::Inspect));
    Ok(())
}
