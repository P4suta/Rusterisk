use super::{CallKey, ChannelEntry, Control, ControlError};
use crate::{call_profiles::NativeProfileCatalog, storage::PreparedMusic, NativeCallProfile};
use rusterisk_core::{SettingsGeneration, SettingsReference};

#[derive(Clone)]
pub(super) struct CallSettings {
    pub(super) native_profiles: Option<NativeProfileCatalog>,
    pub(super) music_catalog: Option<PreparedMusic>,
}

impl CallSettings {
    pub(super) fn empty() -> Self {
        Self {
            native_profiles: None,
            music_catalog: None,
        }
    }

    pub(super) fn configured(config: &rusterisk_config::Config) -> Self {
        Self {
            native_profiles: Some(NativeProfileCatalog::new(config)),
            music_catalog: None,
        }
    }
}

pub(crate) struct NativeStartCapsule {
    key: CallKey,
    generation: SettingsGeneration,
    profile: Result<NativeCallProfile, ControlError>,
}

impl NativeStartCapsule {
    fn matches(&self, key: &CallKey, generation: SettingsGeneration) -> bool {
        self.key == *key && self.generation == generation
    }

    pub(crate) fn into_profile(self) -> Result<NativeCallProfile, ControlError> {
        self.profile
    }
}

impl Control {
    pub(crate) fn server_info(&self) -> &crate::server_info::ServerInfo {
        &self.info
    }

    pub(super) fn publish_call_settings(
        &mut self,
        settings: CallSettings,
    ) -> Result<SettingsGeneration, ControlError> {
        let candidate = self.settings.prepare(settings)?;
        match self.settings.commit(candidate) {
            Ok(generation) => Ok(generation),
            Err(primary) => match self.settings.cancel(candidate) {
                Ok(()) => Err(primary),
                Err(cleanup) => Err(ControlError::Adapter(format!(
                    "{primary}; settings candidate cleanup failed: {cleanup}"
                ))),
            },
        }
    }

    pub(crate) fn bind_native_profiles(
        &mut self,
        bindings: crate::call_profiles::BoundNativeProfiles,
    ) -> Result<(), ControlError> {
        let current = self.settings.get(self.settings.current())?;
        let catalog = current
            .native_profiles
            .as_ref()
            .ok_or(ControlError::Conflict("Native profiles are not configured"))?;
        if catalog.is_bound_to(&bindings)? {
            return Ok(());
        }
        let mut replacement = current.clone();
        replacement
            .native_profiles
            .as_mut()
            .ok_or(ControlError::Conflict("Native profiles are not configured"))?
            .bind(bindings)?;
        self.publish_call_settings(replacement)?;
        Ok(())
    }

    pub(super) fn native_capsule(
        &self,
        key: &CallKey,
        generation: SettingsGeneration,
        endpoint: &str,
    ) -> Result<NativeStartCapsule, ControlError> {
        let settings = self.settings.get(generation)?;
        Ok(NativeStartCapsule {
            key: key.clone(),
            generation,
            profile: settings
                .native_profiles
                .as_ref()
                .ok_or(ControlError::NotFound("Endpoint not found"))
                .and_then(|catalog| catalog.capture(endpoint)),
        })
    }

    pub(super) fn channel_settings(
        &self,
        id: &rusterisk_api::ChannelId,
    ) -> Result<&CallSettings, ControlError> {
        let entry = self
            .channels
            .get(id)
            .ok_or(ControlError::NotFound("Channel not found"))?;
        self.settings.get(entry.settings.generation())
    }

    fn native_entry(
        &mut self,
        key: &CallKey,
        generation: SettingsGeneration,
    ) -> Result<&mut ChannelEntry, ControlError> {
        self.settings.get(generation)?;
        self.channels
            .get_mut(&key.id)
            .filter(|entry| {
                entry.call.generation() == key.generation
                    && entry.settings.generation() == generation
            })
            .ok_or(ControlError::Conflict("Native start generation changed"))
    }

    pub(crate) fn take_native_profile(
        &mut self,
        key: &CallKey,
        generation: SettingsGeneration,
    ) -> Result<NativeStartCapsule, ControlError> {
        let entry = self.native_entry(key, generation)?;
        if !entry
            .pending_native
            .as_ref()
            .is_some_and(|capsule| capsule.matches(key, generation))
        {
            return Err(ControlError::Conflict("Native profile already claimed"));
        }
        entry
            .pending_native
            .take()
            .ok_or(ControlError::Conflict("Native profile already claimed"))
    }

    pub(crate) fn restore_native_profile(
        &mut self,
        key: &CallKey,
        generation: SettingsGeneration,
        capsule: NativeStartCapsule,
    ) -> Result<(), ControlError> {
        if !capsule.matches(key, generation) {
            return Err(ControlError::Conflict("Native capsule generation changed"));
        }
        let entry = self.native_entry(key, generation)?;
        if entry.pending_native.is_some() {
            return Err(ControlError::Conflict("Native profile already pending"));
        }
        entry.pending_native = Some(capsule);
        Ok(())
    }

    pub(super) fn release_settings(
        &mut self,
        references: impl IntoIterator<Item = SettingsReference>,
        primary: ControlError,
    ) -> ControlError {
        let mut failure = primary;
        for reference in references {
            if let Err(cleanup) = self.settings.release(reference) {
                failure = ControlError::Adapter(format!(
                    "{failure}; settings reference cleanup failed: {cleanup}"
                ));
            }
        }
        failure
    }
}
