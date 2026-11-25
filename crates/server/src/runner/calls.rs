use crate::call_records::{CallRecord, Field};
use tokio::sync::mpsc;

#[derive(Default)]
pub(super) struct Resources {
    admission: Option<super::external_admission::Pending>,
    answer: Option<super::answer::Pending>,
    connection: Option<mpsc::Receiver<()>>,
    lifetime: Option<mpsc::Receiver<()>>,
}

impl CallRecord for Resources {
    fn empty(&self) -> bool {
        self.admission.is_none()
            && self.answer.is_none()
            && self.connection.is_none()
            && self.lifetime.is_none()
    }
}

pub(super) const ADMISSION: Field<Resources, super::external_admission::Pending> =
    Field::new(|row| &row.admission, |row| &mut row.admission);
pub(super) const ANSWER: Field<Resources, super::answer::Pending> =
    Field::new(|row| &row.answer, |row| &mut row.answer);
pub(super) const CONNECTION: Field<Resources, mpsc::Receiver<()>> =
    Field::new(|row| &row.connection, |row| &mut row.connection);
pub(super) const LIFETIME: Field<Resources, mpsc::Receiver<()>> =
    Field::new(|row| &row.lifetime, |row| &mut row.lifetime);
