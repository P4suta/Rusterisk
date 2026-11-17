use super::{configuration, start};
use crate::{Control, ControlError};

#[test]
fn an_unprepared_websocket_snapshot_has_an_admission_error() -> Result<(), ControlError> {
    let config = configuration("starting")?;
    let mut control = Control::with_configuration(4, &config);
    control.subscribe("calls")?;
    let (key, generation) = start(&mut control)?;
    let error = control
        .take_websocket_profile(&key, generation)?
        .into_profile()
        .err()
        .ok_or(ControlError::Invalid(
            "Expected an unprepared profile failure",
        ))?;
    assert!(matches!(error, ControlError::Invalid(_)), "{error}");
    Ok(())
}
