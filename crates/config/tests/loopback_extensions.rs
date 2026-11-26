use rusterisk_config::{Application, Config, ConfigError};
use std::path::Path;

#[test]
fn loopback_extensions_route_to_registered_phones_and_the_calls_app() -> Result<(), ConfigError> {
    let config = Config::load(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config"))?;
    let program = &config.dialplan;
    let application = |number: &str| {
        program
            .position("internal", number, 1)
            .map(|position| &program.steps[usize::from(position)].application)
    };
    for (number, destination) in [("101", "PJSIP/alice"), ("102", "PJSIP/bob")] {
        assert!(
            matches!(application(number), Some(Application::Dial { destinations, .. }) if destinations == &[destination]),
            "extension {number} must dial {destination}"
        );
    }
    assert!(matches!(
        application("100"),
        Some(Application::Stasis { application, arguments }) if application == "calls" && arguments.is_empty()
    ));
    Ok(())
}
