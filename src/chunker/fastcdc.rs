//! Thin wrapper around the third-party FastCDC implementation.
//!
//! Oni keeps its own config and chunk types here so the rest of the codebase is
//! not coupled directly to the external crate.

use std::io::Read;

use fastcdc::v2020::{self, StreamCDC};

use crate::error::ChunkerError;

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct Config {
    min_size: u32,
    avg_size: u32,
    max_size: u32,
}

impl Config {
    /// Validate one FastCDC size triple before the rest of the code sees it.
    pub fn new(min_size: u32, avg_size: u32, max_size: u32) -> Result<Self, ChunkerError> {
        validate_bounds("min-size", min_size, v2020::MINIMUM_MIN, v2020::MINIMUM_MAX)?;
        validate_bounds("avg-size", avg_size, v2020::AVERAGE_MIN, v2020::AVERAGE_MAX)?;
        validate_bounds("max-size", max_size, v2020::MAXIMUM_MIN, v2020::MAXIMUM_MAX)?;

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
}

impl Default for Config {
    fn default() -> Self {
        Self {
            min_size: 8 * 1024,
            avg_size: 16 * 1024,
            max_size: 64 * 1024,
        }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct Chunk {
    /// Byte offset of the chunk start in the original input stream.
    pub offset: u64,
    /// Number of bytes in this chunk.
    pub length: usize,
}

/// Chunk an in-memory buffer.
pub fn chunk_bytes(data: &[u8], config: Config) -> Vec<Chunk> {
    v2020::FastCDC::new(
        data,
        config.min_size(),
        config.avg_size(),
        config.max_size(),
    )
    .map(|chunk| Chunk {
        offset: chunk.offset as u64,
        length: chunk.length,
    })
    .collect()
}

/// Chunk a streaming reader.
///
/// Keeping this alongside `chunk_bytes` lets future CDC code choose between
/// in-memory and streaming paths without touching the external crate directly.
pub fn chunk_reader(reader: impl Read, config: Config) -> Result<Vec<Chunk>, ChunkerError> {
    let mut chunks = Vec::new();

    for result in StreamCDC::new(
        reader,
        config.min_size(),
        config.avg_size(),
        config.max_size(),
    ) {
        let chunk = result.map_err(|source| ChunkerError::FastCdcIo {
            operation: "read chunk boundaries from streaming FastCDC source",
            source,
        })?;
        chunks.push(Chunk {
            offset: chunk.offset,
            length: chunk.length,
        });
    }

    Ok(chunks)
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

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::{chunk_bytes, chunk_reader, Config};
    use crate::error::ChunkerError;

    #[test]
    fn rejects_invalid_config_ordering() {
        let error = Config::new(16 * 1024, 8 * 1024, 64 * 1024).unwrap_err();

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
        let chunks = chunk_bytes(&data, Config::default());

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
        let config = Config::default();

        let from_bytes = chunk_bytes(&data, config);
        let from_reader = chunk_reader(Cursor::new(&data), config).unwrap();

        assert_eq!(from_reader, from_bytes);
    }

    #[test]
    fn empty_inputs_produce_no_chunks() {
        let chunks = chunk_bytes(&[], Config::default());

        assert!(chunks.is_empty());
    }
}
