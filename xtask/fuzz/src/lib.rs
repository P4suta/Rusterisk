mod control;
#[cfg(feature = "media")]
mod packets;
#[cfg(feature = "signaling")]
mod registration_responses;
mod settings;
#[cfg(feature = "signaling")]
mod signaling;
#[cfg(any(feature = "signaling", feature = "media"))]
mod stream;
mod targets;

pub use control::controls;
#[cfg(feature = "media")]
pub use packets::media;
#[cfg(feature = "signaling")]
pub use registration_responses::registration_responses;
pub use settings::settings;
#[cfg(feature = "signaling")]
pub use signaling::sip;
pub use targets::{Seed, Target, MAX_INPUT};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OracleError {
    InputCapacity,
    SettingsChanged,
    ControlEncoding,
    ControlChanged,
    Channel,
    Progress,
    StreamChanged,
    SessionEncoding,
    SessionChanged,
    SignalingCapacity,
    RegistrationSetup,
    RegistrationReceipt,
    RegistrationClock,
    RegistrationDeadline,
    RegistrationCancellation,
    RegistrationSettlement,
    AudioEncoding,
    AudioChanged,
    PacketEncoding,
    PacketChanged,
    TelephoneChanged,
}
