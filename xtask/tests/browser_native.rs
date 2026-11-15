use serde_json::{json, Value};
use xtask::browser::{validate_execution, validate_inventory};

fn array<const N: usize>(values: [Value; N]) -> Value {
    Value::Array(values.into_iter().collect())
}

fn report(executed: bool) -> Value {
    let titles = [
        ("calls.spec.ts", "actual outgoing and incoming browser calls exchange known audio and save a recording"),
        ("calls.spec.ts", "cancelling or disconnecting during native capture releases late tracks and preserves reconnect"),
        ("layout.spec.ts", "browser controls preserve monochrome reading, keyboard focus, targets, reflow, enlarged text, and spacing"),
        ("layout.spec.ts", "disabled controls explain unavailable JavaScript and a failed application bundle"),
    ];
    let suites: Vec<_> = titles.iter().enumerate().map(|(index, (file, title))| {
        let results = if executed { array([json!({ "status": "passed", "retry": 0, "errors": [] })]) } else { array([]) };
        let tests = array([json!({ "expectedStatus": "passed", "status": if executed { "expected" } else { "skipped" }, "annotations": [], "results": results })]);
        let specs = array([json!({ "file": file, "title": title, "id": format!("case-{index}"), "ok": true, "tests": tests })]);
        json!({ "file": file, "specs": specs })
    }).collect();
    let projects = array([json!({ "retries": 0, "repeatEach": 1, "timeout": 60000 })]);
    let filtered_projects = array([
        json!({ "use": { "browserName": "chromium", "channel": "chromium", "headless": true, "trace": "off", "video": "off", "screenshot": "off" } }),
    ]);
    json!({
        "config": { "version": "1.65.1", "workers": 1, "forbidOnly": true, "fullyParallel": false, "globalTimeout": 120000, "projects": projects, "filteredProjects": filtered_projects },
        "suites": suites,
        "errors": [],
        "stats": { "duration": 1000, "expected": if executed { 4 } else { 0 }, "skipped": if executed { 0 } else { 4 }, "unexpected": 0, "flaky": 0 },
    })
}

#[test]
fn browser_native_reports_require_complete_single_attempt_execution_without_skips_or_retries(
) -> Result<(), std::io::Error> {
    let inventory =
        validate_inventory(&serde_json::to_vec(&report(false))?).map_err(std::io::Error::other)?;
    assert_eq!(
        validate_execution(&serde_json::to_vec(&report(true))?, &inventory)
            .map_err(std::io::Error::other)?,
        4
    );
    for pointer in [
        "/config/workers",
        "/config/projects/0/retries",
        "/config/projects/0/repeatEach",
        "/config/globalTimeout",
    ] {
        let mut refused = report(false);
        *refused
            .pointer_mut(pointer)
            .ok_or_else(|| std::io::Error::other("missing fixture field"))? = json!(2);
        assert!(
            validate_inventory(&serde_json::to_vec(&refused)?).is_err(),
            "{pointer}"
        );
    }
    for (pointer, replacement) in [
        ("/config/version", json!("other")),
        ("/config/forbidOnly", json!(false)),
        ("/config/filteredProjects", json!([])),
        (
            "/config/filteredProjects/0/use/browserName",
            json!("firefox"),
        ),
        (
            "/config/filteredProjects/0/use/channel",
            json!("chrome-headless-shell"),
        ),
        ("/config/filteredProjects/0/use/channel", json!(null)),
        ("/config/filteredProjects/0/use/headless", json!(false)),
        ("/config/filteredProjects/0/use/trace", json!("on")),
        ("/config/filteredProjects/0/use/video", json!("on")),
        ("/config/filteredProjects/0/use/screenshot", json!("on")),
        ("/suites", json!([])),
        ("/suites/0/specs/0/tests/0/expectedStatus", json!("skipped")),
        (
            "/errors",
            array([json!({"message":"native fixture failed"})]),
        ),
    ] {
        let mut refused = report(false);
        *refused
            .pointer_mut(pointer)
            .ok_or_else(|| std::io::Error::other("missing fixture field"))? = replacement;
        assert!(
            validate_inventory(&serde_json::to_vec(&refused)?).is_err(),
            "{pointer}"
        );
    }
    for (pointer, replacement) in [
        ("/suites/0/specs/0/tests/0/results", json!([])),
        (
            "/suites/0/specs/0/tests/0/results/0/status",
            json!("timedOut"),
        ),
        ("/suites/0/specs/0/tests/0/results/0/retry", json!(1)),
        ("/suites/0/specs/0/tests/0/status", json!("skipped")),
        ("/suites/0/specs/0/ok", json!(false)),
        ("/stats/skipped", json!(1)),
        ("/stats/unexpected", json!(1)),
        ("/stats/flaky", json!(1)),
        ("/stats/expected", json!(0)),
        ("/suites/0/specs/0/id", json!("different-source")),
    ] {
        let mut refused = report(true);
        *refused
            .pointer_mut(pointer)
            .ok_or_else(|| std::io::Error::other("missing fixture field"))? = replacement;
        assert!(
            validate_execution(&serde_json::to_vec(&refused)?, &inventory).is_err(),
            "{pointer}"
        );
    }
    Ok(())
}
