//! Framed stdio transport used by local and SSH-spawned helpers.

use std::io::{Read, Write};

use crate::error::TransportError;

/// Length-prefixed framing over stdin/stdout.
///
/// The transport layer only moves opaque payload bytes. Message encoding lives
/// in `protocol`, so transport stays reusable for local subprocesses and SSH.
pub struct Connection<R, W> {
    reader: R,
    writer: W,
    max_frame_bytes: usize,
}

impl<R, W> Connection<R, W> {
    pub fn new(reader: R, writer: W, max_frame_bytes: usize) -> Self {
        Self {
            reader,
            writer,
            max_frame_bytes,
        }
    }
}

impl<R: Read, W: Write> Connection<R, W> {
    pub fn send_frame(&mut self, payload: &[u8]) -> Result<(), TransportError> {
        // The frame format is:
        // - 4-byte big-endian length prefix
        // - raw payload bytes
        if payload.len() > self.max_frame_bytes {
            return Err(TransportError::FrameTooLarge {
                len: payload.len(),
                max: self.max_frame_bytes,
            });
        }

        let frame_len =
            u32::try_from(payload.len()).map_err(|_| TransportError::FrameTooLarge {
                len: payload.len(),
                max: self.max_frame_bytes,
            })?;

        self.writer
            .write_all(&frame_len.to_be_bytes())
            .map_err(|source| TransportError::Io {
                operation: "write frame length",
                source,
            })?;
        self.writer
            .write_all(payload)
            .map_err(|source| TransportError::Io {
                operation: "write frame payload",
                source,
            })?;
        self.writer.flush().map_err(|source| TransportError::Io {
            operation: "flush framed output",
            source,
        })?;

        Ok(())
    }

    pub fn receive_frame(&mut self) -> Result<Option<Vec<u8>>, TransportError> {
        // `None` means clean EOF before a new frame started.
        // Any EOF after a partial prefix becomes an explicit transport error.
        let Some(length_prefix) = self.read_length_prefix()? else {
            return Ok(None);
        };

        let frame_len = u32::from_be_bytes(length_prefix) as usize;
        if frame_len > self.max_frame_bytes {
            return Err(TransportError::FrameTooLarge {
                len: frame_len,
                max: self.max_frame_bytes,
            });
        }

        let mut payload = vec![0_u8; frame_len];
        self.reader
            .read_exact(&mut payload)
            .map_err(|source| TransportError::Io {
                operation: "read frame payload",
                source,
            })?;

        Ok(Some(payload))
    }

    fn read_length_prefix(&mut self) -> Result<Option<[u8; 4]>, TransportError> {
        let mut prefix = [0_u8; 4];
        let mut offset = 0;

        while offset < prefix.len() {
            // Plain `read_exact` would collapse clean EOF and truncated prefix into
            // the same error. This loop keeps those cases distinct.
            let read =
                self.reader
                    .read(&mut prefix[offset..])
                    .map_err(|source| TransportError::Io {
                        operation: "read frame length",
                        source,
                    })?;

            if read == 0 {
                if offset == 0 {
                    return Ok(None);
                }

                return Err(TransportError::UnexpectedEof {
                    operation: "read frame length",
                });
            }

            offset += read;
        }

        Ok(Some(prefix))
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::Connection;
    use crate::error::TransportError;

    #[test]
    fn round_trips_one_frame() {
        let mut encoded = Vec::new();
        Connection::new(Cursor::new(Vec::<u8>::new()), &mut encoded, 1024)
            .send_frame(b"oni")
            .unwrap();

        let mut connection = Connection::new(Cursor::new(encoded), Vec::<u8>::new(), 1024);
        let frame = connection.receive_frame().unwrap().unwrap();

        assert_eq!(frame, b"oni");
        assert!(connection.receive_frame().unwrap().is_none());
    }

    #[test]
    fn rejects_frames_larger_than_the_configured_limit() {
        let error = Connection::new(Cursor::new(Vec::<u8>::new()), Vec::<u8>::new(), 2)
            .send_frame(b"oni")
            .unwrap_err();

        match error {
            TransportError::FrameTooLarge { len, max } => {
                assert_eq!(len, 3);
                assert_eq!(max, 2);
            }
            other => panic!("expected frame-too-large error, got {other:?}"),
        }
    }

    #[test]
    fn rejects_truncated_length_prefixes() {
        let mut connection = Connection::new(Cursor::new(vec![0, 0]), Vec::<u8>::new(), 1024);
        let error = connection.receive_frame().unwrap_err();

        match error {
            TransportError::UnexpectedEof { operation } => {
                assert_eq!(operation, "read frame length");
            }
            other => panic!("expected unexpected-eof error, got {other:?}"),
        }
    }
}
