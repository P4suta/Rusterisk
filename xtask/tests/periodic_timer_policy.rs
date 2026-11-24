use std::path::Path;
use xtask::timer_policy::{inspect, production, Timer};

fn timer(item: &str) -> Timer {
    Timer {
        path: "crates/server/src/link.rs".into(),
        item: item.into(),
        adr: "docs/adr/wake-each-task-at-its-earliest-deadline.md".into(),
        rationale: "Pace media frames while a call exists.".into(),
    }
}

const PACED: &str = r"
struct Link;
impl Link {
    async fn serve(&mut self) {
        let mut tick = tokio::time::interval_at(tokio::time::Instant::now(), period());
        tick.tick().await;
    }
}
";

fn rules(source: &str, timers: &[Timer]) -> Result<Vec<&'static str>, syn::Error> {
    Ok(inspect(source, timers)?
        .into_iter()
        .map(|finding| finding.rule)
        .collect())
}

#[test]
fn permits_a_periodic_timer_in_its_registered_function() -> Result<(), syn::Error> {
    assert!(rules(PACED, &[timer("serve")])?.is_empty());
    Ok(())
}

#[test]
fn rejects_a_periodic_timer_without_a_registration() -> Result<(), syn::Error> {
    assert_eq!(rules(PACED, &[])?, ["periodic-timer"]);
    assert_eq!(
        rules(PACED, &[timer("other")])?,
        ["periodic-timer", "unused-timer"]
    );
    Ok(())
}

#[test]
fn rejects_a_second_timer_under_one_registration() -> Result<(), syn::Error> {
    let source = r"
        async fn serve() {
            let mut frames = tokio::time::interval(period());
            let mut reports = tokio::time::interval(period());
        }
    ";
    assert_eq!(rules(source, &[timer("serve")])?, ["periodic-timer"]);
    assert!(rules(source, &[timer("serve"), timer("serve")])?.is_empty());
    Ok(())
}

#[test]
fn rejects_a_registration_that_no_timer_uses() -> Result<(), syn::Error> {
    assert_eq!(rules("fn idle() {}", &[timer("idle")])?, ["unused-timer"]);
    Ok(())
}

#[test]
fn rejects_imported_aliased_and_macro_timers() -> Result<(), syn::Error> {
    for source in [
        "use tokio::time::interval; fn run() { let _ticks = interval(period()); }",
        "use tokio::time::interval as every; fn run() {}",
        "use tokio::time::{interval_at as every}; fn run() {}",
        "async fn run() { tokio::select! { _ = tokio::time::interval(period()).tick() => {} } }",
        "fn run() { let start = || tokio::time::interval_at(now(), period()); }",
    ] {
        assert_eq!(rules(source, &[])?, ["periodic-timer"], "{source}");
    }
    Ok(())
}

#[test]
fn ignores_a_timer_inside_a_test_module() -> Result<(), syn::Error> {
    let source = r"
        #[cfg(test)]
        mod tests {
            async fn clock() {
                let mut ticks = tokio::time::interval(period());
            }
        }
    ";
    assert!(rules(source, &[])?.is_empty());
    Ok(())
}

#[test]
fn covers_production_sources_and_leaves_tests_and_examples() {
    for (path, covered) in [
        ("crates/server/src/rtp.rs", true),
        ("xtask/src/main.rs", true),
        ("crates/server/src/rtp/control_tests.rs", false),
        ("crates/server/src/signaling/registrations/tests.rs", false),
        (
            "crates/server/src/signaling/registrations/tests/pacing.rs",
            false,
        ),
        ("crates/server/tests/rtp_routes.rs", false),
        ("crates/client/examples/external_audio.rs", false),
    ] {
        assert_eq!(production(Path::new(path)), covered, "{path}");
    }
}
