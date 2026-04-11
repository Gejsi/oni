//! Oni-owned FastCDC boundary.
//!
//! The current implementation still delegates to the third-party crate, but the
//! rest of Oni consumes callback-based chunk streams from this module instead
//! of depending on the crate's `Vec`- or iterator-shaped APIs directly.
//!
//! Important distinction:
//! - at Oni's API boundary, `chunk_slice(...)` and `chunk_read(...)` are
//!   streaming because they deliver one chunk at a time to a callback instead
//!   of materializing a `Vec<Chunk>`
//! - inside `chunk_read(...)`, we still use the upstream `StreamCDC`
//!   adapter, which allocates an owned `Vec<u8>` for each yielded chunk
//!
//! That means the boundary is now correct for the rest of Oni, but the
//! implementation is not yet the final no-per-chunk-allocation hot path.
//! Vendoring the FastCDC core only becomes justified once profiling or the next
//! CDC refactor needs borrowed slices from one reusable buffer or direct
//! token-emission while chunking.

use std::io::Read;

use fastcdc::v2020::{self, StreamCDC};

use crate::error::ChunkerError;

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct FastCdcConfig {
    min_size: u32,
    avg_size: u32,
    max_size: u32,
}

impl FastCdcConfig {
    /// Validate one FastCDC size triple before the rest of the code sees it.
    pub fn new(min_size: u32, avg_size: u32, max_size: u32) -> Result<Self, ChunkerError> {
        Self::validate_bounds("min-size", min_size, v2020::MINIMUM_MIN, v2020::MINIMUM_MAX)?;
        Self::validate_bounds("avg-size", avg_size, v2020::AVERAGE_MIN, v2020::AVERAGE_MAX)?;
        Self::validate_bounds("max-size", max_size, v2020::MAXIMUM_MIN, v2020::MAXIMUM_MAX)?;

        if !(min_size < avg_size && avg_size < max_size) {
            return Err(ChunkerError::InvalidOrdering {
                min_size,
                avg_size,
                max_size,
            });
        }

        Ok(Self {
            min_size,
            avg_size,
            max_size,
        })
    }

    pub fn min_size(self) -> u32 {
        self.min_size
    }

    pub fn avg_size(self) -> u32 {
        self.avg_size
    }

    pub fn max_size(self) -> u32 {
        self.max_size
    }

    fn validate_bounds(
        field: &'static str,
        value: u32,
        min: u32,
        max: u32,
    ) -> Result<(), ChunkerError> {
        if value < min || value > max {
            return Err(ChunkerError::InvalidBound {
                field,
                value,
                min,
                max,
            });
        }

        Ok(())
    }
}

impl Default for FastCdcConfig {
    fn default() -> Self {
        Self {
            min_size: 8 * 1024,
            avg_size: 16 * 1024,
            max_size: 64 * 1024,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct Chunk {
    /// Rolling FastCDC hash at the end of this chunk.
    pub hash: u64,
    /// Byte offset of the chunk start in the original input stream.
    pub offset: u64,
    /// Number of bytes in this chunk.
    pub length: usize,
}

/// Chunk an in-memory slice.
///
/// Useful for mmap-backed local file fast paths because the
/// caller already owns one contiguous readable view of the input bytes.
pub fn chunk_slice<E, F>(data: &[u8], config: FastCdcConfig, mut on_chunk: F) -> Result<(), E>
where
    E: From<ChunkerError>,
    F: FnMut(Chunk, &[u8]) -> Result<(), E>,
{
    for chunk in v2020::FastCDC::new(
        data,
        config.min_size(),
        config.avg_size(),
        config.max_size(),
    ) {
        let start = chunk.offset;
        let end = start + chunk.length;
        on_chunk(
            Chunk {
                hash: chunk.hash,
                offset: chunk.offset as u64,
                length: chunk.length,
            },
            &data[start..end],
        )?;
    }

    Ok(())
}

/// Chunk a generic streaming reader.
///
/// This is the path for SSH stdio, pipes, sockets, and any other source that
/// cannot expose a stable full slice.
///
/// This is streaming at Oni's API boundary, but not yet the final internal hot
/// path because the current upstream adapter still hands us owned chunk
/// buffers.
pub fn chunk_read<R, E, F>(reader: &mut R, config: FastCdcConfig, mut on_chunk: F) -> Result<(), E>
where
    R: Read,
    E: From<ChunkerError>,
    F: FnMut(Chunk, &[u8]) -> Result<(), E>,
{
    for result in StreamCDC::new(
        &mut *reader,
        config.min_size(),
        config.avg_size(),
        config.max_size(),
    ) {
        let chunk = result.map_err(|source| {
            E::from(ChunkerError::FastCdcIo {
                operation: "read streaming FastCDC chunk",
                source,
            })
        })?;
        on_chunk(
            Chunk {
                hash: chunk.hash,
                offset: chunk.offset,
                length: chunk.length,
            },
            &chunk.data,
        )?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::{chunk_read, chunk_slice, Chunk, FastCdcConfig};
    use crate::error::ChunkerError;

    fn collect_slice(data: &[u8], config: FastCdcConfig) -> Vec<Chunk> {
        let mut chunks = Vec::new();
        chunk_slice(data, config, |chunk, _| {
            chunks.push(chunk);
            Ok::<_, ChunkerError>(())
        })
        .unwrap();
        chunks
    }

    fn collect_reader(data: &[u8], config: FastCdcConfig) -> Vec<Chunk> {
        let mut chunks = Vec::new();
        let mut reader = Cursor::new(data);
        chunk_read(&mut reader, config, |chunk, _| {
            chunks.push(chunk);
            Ok::<_, ChunkerError>(())
        })
        .unwrap();
        chunks
    }

    #[test]
    fn rejects_invalid_config_ordering() {
        let error = FastCdcConfig::new(16 * 1024, 8 * 1024, 64 * 1024).unwrap_err();

        match error {
            ChunkerError::InvalidOrdering {
                min_size,
                avg_size,
                max_size,
            } => {
                assert_eq!(min_size, 16 * 1024);
                assert_eq!(avg_size, 8 * 1024);
                assert_eq!(max_size, 64 * 1024);
            }
            other => panic!("expected invalid-ordering error, got {other}"),
        }
    }

    #[test]
    fn chunks_cover_the_input_without_gaps() {
        let data = vec![b'a'; 256 * 1024];
        let chunks = collect_slice(&data, FastCdcConfig::default());

        assert!(!chunks.is_empty());
        assert_eq!(chunks.first().unwrap().offset, 0);

        let mut expected_offset = 0_u64;
        for chunk in &chunks {
            assert_eq!(chunk.offset, expected_offset);
            expected_offset += chunk.length as u64;
        }

        assert_eq!(expected_offset, data.len() as u64);
    }

    #[test]
    fn streaming_and_in_memory_chunkers_agree() {
        let data = (0..200_000)
            .map(|index| (index % 251) as u8)
            .collect::<Vec<_>>();
        let config = FastCdcConfig::default();

        let from_slice = collect_slice(&data, config);
        let from_reader = collect_reader(&data, config);

        assert_eq!(from_reader, from_slice);
    }

    #[test]
    fn callback_receives_chunk_bytes_in_order() {
        let data = (0..80_000)
            .map(|index| (index % 241) as u8)
            .collect::<Vec<_>>();
        let config = FastCdcConfig::default();
        let mut rebuilt = Vec::new();

        chunk_slice(&data, config, |chunk, bytes| {
            assert_eq!(bytes.len(), chunk.length);
            rebuilt.extend_from_slice(bytes);
            Ok::<_, ChunkerError>(())
        })
        .unwrap();

        assert_eq!(rebuilt, data);
    }

    #[test]
    fn empty_inputs_produce_no_chunks() {
        let chunks = collect_slice(&[], FastCdcConfig::default());

        assert!(chunks.is_empty());
    }
}
