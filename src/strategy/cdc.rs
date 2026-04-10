//! Content-defined chunking delta primitives.
//!
//! This baseline keeps the CDC path transport-agnostic:
//! - chunk an existing destination file through Oni's FastCDC boundary
//! - hash each destination chunk strongly
//! - chunk the source file with the same FastCDC parameters
//! - turn each source chunk into either a destination reference or a literal write
//! - stream those decisions directly into a sink
//!
//!   destination file --chunk_read--> chunk bytes --hash--> signature table
//!   source file --chunk_read--> chunk bytes --hash--> reference/literal sink
//!   sink + destination file --------------------------------> rebuilt output

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom, Write};

use crate::chunker::fastcdc::{self, FastCdcConfig};
use crate::error::StrategyError;

const READ_BUFFER_SIZE: usize = 8 * 1024;

#[derive(Debug, PartialEq, Eq)]
struct ChunkSignature {
    /// Byte offset of the matching span in the destination file.
    offset: u64,
    /// Length of that span. FastCDC chunk lengths are variable.
    len: usize,
}

#[derive(Debug, PartialEq, Eq)]
pub struct SignatureTable {
    /// Destination chunks in file order. A later reference points back into this set.
    chunks: Vec<ChunkSignature>,
    /// Strong-hash index used to find candidate destination chunks for one source chunk.
    /// The value is a vector because duplicate chunk contents can appear more
    /// than once in the destination file.
    by_hash: HashMap<[u8; 32], Vec<usize>>,
}

/// One per-file summary of what the source pass emitted.
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq)]
pub struct DeltaStats {
    pub reference_chunks: usize,
    pub literal_chunks: usize,
    pub literal_bytes: u64,
}

/// Destination for source-side CDC decisions.
///
/// reference: reuse a span from the destination file
/// literal: write new bytes from the source
pub trait DeltaSink {
    fn literal(&mut self, bytes: &[u8]) -> Result<(), StrategyError>;
    fn reference(&mut self, offset: u64, len: usize) -> Result<(), StrategyError>;
}

pub fn signatures_fastcdc(
    reader: &mut impl Read,
    config: FastCdcConfig,
) -> Result<SignatureTable, StrategyError> {
    // Build one bounded per-file destination index:
    // - `chunks` keeps the original offset/length for each destination chunk
    // - `by_hash` lets the source pass jump from one strong hash to candidate
    //   destination spans without rescanning the whole destination file
    let mut chunks = Vec::new();
    let mut by_hash = HashMap::new();

    fastcdc::chunk_read(reader, config, |chunk, bytes| {
        let strong = strong_checksum(bytes);
        by_hash
            .entry(strong)
            .or_insert_with(Vec::new)
            .push(chunks.len());
        chunks.push(ChunkSignature {
            offset: chunk.offset,
            len: chunk.length,
        });
        Ok::<_, StrategyError>(())
    })?;

    Ok(SignatureTable { chunks, by_hash })
}

/// Stream one source file through the FastCDC matcher and count the emitted
/// decisions without retaining them.
pub fn emit_delta_fastcdc(
    reader: &mut impl Read,
    signatures: &SignatureTable,
    config: FastCdcConfig,
) -> Result<DeltaStats, StrategyError> {
    emit_delta_fastcdc_into(reader, signatures, config, &mut DiscardingSink)
}

/// Stream one source file through the FastCDC matcher and emit destination
/// references or literal bytes immediately.
///
/// This keeps the source side single-pass and lets callers decide how to apply
/// or forward the delta decisions.
pub fn emit_delta_fastcdc_into(
    reader: &mut impl Read,
    signatures: &SignatureTable,
    config: FastCdcConfig,
    sink: &mut impl DeltaSink,
) -> Result<DeltaStats, StrategyError> {
    let mut stats = DeltaStats::default();

    fastcdc::chunk_read(reader, config, |chunk, bytes| {
        let strong = strong_checksum(bytes);

        if let Some(signature) = find_match(signatures, strong, chunk.length) {
            sink.reference(signature.offset, signature.len)?;
            stats.reference_chunks += 1;
        } else {
            sink.literal(bytes)?;
            stats.literal_chunks += 1;
            stats.literal_bytes += bytes.len() as u64;
        }

        Ok::<_, StrategyError>(())
    })?;

    Ok(stats)
}

/// Apply one source file directly into `writer` by rereading referenced spans
/// from `destination` on demand.
///
/// This is the local stepping stone toward the future helper-backed flow:
/// signatures stay buffered, but source decisions stream straight into the temp
/// file writer instead of waiting behind an in-memory recipe.
pub fn apply_fastcdc(
    reader: &mut impl Read,
    signatures: &SignatureTable,
    config: FastCdcConfig,
    destination: &mut (impl Read + Seek),
    writer: &mut impl Write,
) -> Result<DeltaStats, StrategyError> {
    let mut sink = ApplySink::new(destination, writer);
    emit_delta_fastcdc_into(reader, signatures, config, &mut sink)
}

struct DiscardingSink;

impl DeltaSink for DiscardingSink {
    fn literal(&mut self, _bytes: &[u8]) -> Result<(), StrategyError> {
        Ok(())
    }

    fn reference(&mut self, _offset: u64, _len: usize) -> Result<(), StrategyError> {
        Ok(())
    }
}

struct ApplySink<'a, R, W> {
    destination: &'a mut R,
    writer: &'a mut W,
    buffer: [u8; READ_BUFFER_SIZE],
}

impl<'a, R, W> ApplySink<'a, R, W>
where
    R: Read + Seek,
    W: Write,
{
    fn new(destination: &'a mut R, writer: &'a mut W) -> Self {
        Self {
            destination,
            writer,
            buffer: [0_u8; READ_BUFFER_SIZE],
        }
    }
}

impl<R, W> DeltaSink for ApplySink<'_, R, W>
where
    R: Read + Seek,
    W: Write,
{
    fn literal(&mut self, bytes: &[u8]) -> Result<(), StrategyError> {
        self.writer
            .write_all(bytes)
            .map_err(|source| StrategyError::Io {
                operation: "write literal CDC bytes",
                source,
            })
    }

    fn reference(&mut self, offset: u64, len: usize) -> Result<(), StrategyError> {
        // References intentionally reread the destination file on demand. That keeps
        // the apply path bounded and mirrors the future helper-side execution
        // shape more closely than buffering copied bytes ahead of time.
        self.destination
            .seek(SeekFrom::Start(offset))
            .map_err(|source| StrategyError::Io {
                operation: "seek destination file for CDC reference",
                source,
            })?;

        let mut remaining = len;
        while remaining > 0 {
            let read_len = remaining.min(self.buffer.len());
            let read = self
                .destination
                .read(&mut self.buffer[..read_len])
                .map_err(|source| StrategyError::Io {
                    operation: "read referenced CDC destination span",
                    source,
                })?;

            if read == 0 {
                return Err(StrategyError::InvalidBasisSpan { offset, len });
            }

            self.writer
                .write_all(&self.buffer[..read])
                .map_err(|source| StrategyError::Io {
                    operation: "write referenced CDC bytes",
                    source,
                })?;
            remaining -= read;
        }

        Ok(())
    }
}

fn strong_checksum(bytes: &[u8]) -> [u8; 32] {
    *blake3::hash(bytes).as_bytes()
}

fn find_match<'a>(
    signatures: &'a SignatureTable,
    strong: [u8; 32],
    len: usize,
) -> Option<&'a ChunkSignature> {
    let candidates = signatures.by_hash.get(&strong)?;
    candidates.iter().find_map(|index| {
        let candidate = &signatures.chunks[*index];
        (candidate.len == len).then_some(candidate)
    })
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::{
        apply_fastcdc, emit_delta_fastcdc, emit_delta_fastcdc_into, signatures_fastcdc, DeltaSink,
        FastCdcConfig,
    };
    use crate::error::StrategyError;

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum RecordedChunk {
        Literal(Vec<u8>),
        Reference { offset: u64, len: usize },
    }

    #[derive(Debug, Default)]
    struct RecordingSink {
        chunks: Vec<RecordedChunk>,
    }

    impl DeltaSink for RecordingSink {
        fn literal(&mut self, bytes: &[u8]) -> Result<(), StrategyError> {
            if bytes.is_empty() {
                return Ok(());
            }

            match self.chunks.last_mut() {
                Some(RecordedChunk::Literal(existing)) => existing.extend_from_slice(bytes),
                _ => self.chunks.push(RecordedChunk::Literal(bytes.to_vec())),
            }

            Ok(())
        }

        fn reference(&mut self, offset: u64, len: usize) -> Result<(), StrategyError> {
            self.chunks.push(RecordedChunk::Reference { offset, len });
            Ok(())
        }
    }

    fn patterned_bytes(len: usize) -> Vec<u8> {
        (0..len)
            .map(|index| ((index * 31 + index / 97) % 251) as u8)
            .collect()
    }

    #[test]
    fn identical_files_turn_into_reference_only_streams() {
        let basis_bytes = patterned_bytes(256 * 1024);
        let config = FastCdcConfig::default();

        let signatures = signatures_fastcdc(&mut Cursor::new(&basis_bytes), config).unwrap();
        let mut recorded = RecordingSink::default();
        let stats = emit_delta_fastcdc_into(
            &mut Cursor::new(&basis_bytes),
            &signatures,
            config,
            &mut recorded,
        )
        .unwrap();

        assert!(stats.reference_chunks > 0);
        assert_eq!(stats.literal_chunks, 0);
        assert!(recorded
            .chunks
            .iter()
            .all(|chunk| matches!(chunk, RecordedChunk::Reference { .. })));

        let mut rebuilt = Vec::new();
        let mut basis_reader = Cursor::new(&basis_bytes);
        let apply_stats = apply_fastcdc(
            &mut Cursor::new(&basis_bytes),
            &signatures,
            config,
            &mut basis_reader,
            &mut rebuilt,
        )
        .unwrap();
        assert_eq!(apply_stats.literal_chunks, 0);
        assert_eq!(rebuilt, basis_bytes);
    }

    #[test]
    fn shifted_inserts_preserve_reference_reuse_and_rebuild_the_source() {
        let basis_bytes = patterned_bytes(320 * 1024);
        let mut source_bytes = basis_bytes[..128 * 1024].to_vec();
        source_bytes.extend(std::iter::repeat_n(b'!', 4 * 1024));
        source_bytes.extend_from_slice(&basis_bytes[128 * 1024..]);
        let config = FastCdcConfig::default();

        let signatures = signatures_fastcdc(&mut Cursor::new(&basis_bytes), config).unwrap();
        let mut recorded = RecordingSink::default();
        let stats = emit_delta_fastcdc_into(
            &mut Cursor::new(&source_bytes),
            &signatures,
            config,
            &mut recorded,
        )
        .unwrap();

        assert!(recorded
            .chunks
            .iter()
            .any(|chunk| matches!(chunk, RecordedChunk::Reference { .. })));
        assert!(recorded
            .chunks
            .iter()
            .any(|chunk| matches!(chunk, RecordedChunk::Literal(_))));
        assert!(stats.reference_chunks > 0);
        assert!(stats.literal_chunks > 0);

        let mut rebuilt = Vec::new();
        let mut basis_reader = Cursor::new(&basis_bytes);
        apply_fastcdc(
            &mut Cursor::new(&source_bytes),
            &signatures,
            config,
            &mut basis_reader,
            &mut rebuilt,
        )
        .unwrap();
        assert_eq!(rebuilt, source_bytes);
    }

    #[test]
    fn equal_size_overwrites_still_rebuild_the_source() {
        let basis_bytes = patterned_bytes(192 * 1024);
        let mut source_bytes = basis_bytes.clone();
        source_bytes[64 * 1024..64 * 1024 + 17].copy_from_slice(b"replacement-bytes");
        let config = FastCdcConfig::default();

        let signatures = signatures_fastcdc(&mut Cursor::new(&basis_bytes), config).unwrap();
        let stats =
            emit_delta_fastcdc(&mut Cursor::new(&source_bytes), &signatures, config).unwrap();

        assert!(stats.literal_chunks > 0);

        let mut rebuilt = Vec::new();
        let mut basis_reader = Cursor::new(&basis_bytes);
        apply_fastcdc(
            &mut Cursor::new(&source_bytes),
            &signatures,
            config,
            &mut basis_reader,
            &mut rebuilt,
        )
        .unwrap();
        assert_eq!(rebuilt, source_bytes);
    }
}
