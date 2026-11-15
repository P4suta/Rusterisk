use crate::OracleError;

#[derive(PartialEq, Eq)]
pub struct StreamRead<T, E> {
    pub messages: Vec<T>,
    pub error: Option<E>,
}

pub fn read<T, E>(
    bytes: &[u8],
    chunk: usize,
    mut push: impl FnMut(&[u8]) -> Result<(usize, Option<T>), E>,
) -> Result<StreamRead<T, E>, OracleError> {
    let mut messages = Vec::new();
    for chunk in bytes.chunks(chunk.max(1)) {
        let mut offset = 0;
        while offset < chunk.len() {
            match push(&chunk[offset..]) {
                Ok((consumed, message)) => {
                    if consumed == 0 || consumed > chunk.len() - offset {
                        return Err(OracleError::Progress);
                    }
                    offset += consumed;
                    if let Some(message) = message {
                        messages.push(message);
                    }
                }
                Err(error) => {
                    return Ok(StreamRead {
                        messages,
                        error: Some(error),
                    });
                }
            }
        }
    }
    Ok(StreamRead {
        messages,
        error: None,
    })
}
