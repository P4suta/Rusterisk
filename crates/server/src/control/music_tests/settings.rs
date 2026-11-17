use super::{fixture, request, Action, ControlError, Effect};
use crate::control::native_start::CallSettings;

#[tokio::test]
async fn music_selection_uses_the_call_generation_after_current_settings_change(
) -> Result<(), ControlError> {
    fixture("Main", |control, key| {
        let starting = control.channels[&key.id].settings.generation();
        let current = control.publish_call_settings(CallSettings::empty())?;
        assert_ne!(starting, current);
        let started = control.apply(request(&key, Some("Main")), "start", 0)?;
        assert!(matches!(started.effects.as_slice(), [Effect::StartMusic { selection: Ok(selection), .. }] if selection.class_name() == "Main"));
        control.apply(Action::StopMusic(key.id.clone()), "stop", 1)?;
        control.apply(Action::Hangup(key.id), "end", 2)?;
        assert!(control.settings.get(starting).is_err());
        Ok(())
    }).await
}
