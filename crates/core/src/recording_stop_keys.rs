use crate::DtmfDigit;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RecordingStopKey {
    #[default]
    None,
    Any,
    Star,
    Hash,
}

impl RecordingStopKey {
    pub fn accepts(self, digit: DtmfDigit) -> bool {
        match self {
            Self::None => false,
            Self::Any => true,
            Self::Star => digit == DtmfDigit::Star,
            Self::Hash => digit == DtmfDigit::Hash,
        }
    }
}
