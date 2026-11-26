use crate::call_records::{CallRecord, Field};

#[derive(Default)]
pub(super) struct Resources {
    native: Option<super::outbound::Pending>,
    active: Option<super::Active>,
    media: Option<super::Pending>,
    external: Option<()>,
    stream: Option<super::external_media::PendingStream>,
    websocket: Option<super::websocket::PendingWebSocket>,
}

impl CallRecord for Resources {
    fn empty(&self) -> bool {
        self.native.is_none()
            && self.active.is_none()
            && self.media.is_none()
            && self.external.is_none()
            && self.stream.is_none()
            && self.websocket.is_none()
    }
}

pub(super) const NATIVE: Field<Resources, super::outbound::Pending> =
    Field::new(|row| &row.native, |row| &mut row.native);
pub(super) const ACTIVE: Field<Resources, super::Active> =
    Field::new(|row| &row.active, |row| &mut row.active);
pub(super) const MEDIA: Field<Resources, super::Pending> =
    Field::new(|row| &row.media, |row| &mut row.media);
pub(super) const EXTERNAL: Field<Resources, ()> =
    Field::new(|row| &row.external, |row| &mut row.external);
pub(super) const STREAM: Field<Resources, super::external_media::PendingStream> =
    Field::new(|row| &row.stream, |row| &mut row.stream);
pub(super) const WEBSOCKET: Field<Resources, super::websocket::PendingWebSocket> =
    Field::new(|row| &row.websocket, |row| &mut row.websocket);
