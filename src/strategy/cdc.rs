//! Content-defined chunking delta primitives.
//!
//! This baseline keeps the CDC path transport-agnostic:
//! - chunk an existing basis file with FastCDC
//! - hash each basis chunk strongly
//! - chunk the source file with the same FastCDC parameters
//! - reuse matching basis chunks and emit literals for the rest
//! - replay the recipe against the basis file to reconstruct the source
//!
//! The implementation deliberately stays simple for the first production
//! baseline. It uses two passes per file:
//! - one pass to find chunk boundaries
//! - one pass to read and hash the chunk contents
//!
//! That keeps memory bounded to one chunk at a time while avoiding whole-file
//! buffering in the executor path.
//!
//! ASCII view:
//!
//!   basis file --FastCDC boundaries--> chunk spans --hash--> signature table
//!   source file --FastCDC boundaries-> chunk spans --hash--> copy/literal recipe
//!   recipe + basis file ----------------------------------> rebuilt output
//!
//! The important tradeoff is visible in the diagram: this baseline is simple
//! and bounded, but it is multi-pass. On local same-disk benchmarks that extra
//! work can dominate any bytes saved by the recipe.

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom, Write};

use crate::chunker::fastcdc::{self, Chunk};
use crate::error::StrategyError;

const READ_BUFFER_SIZE: usize = 8 * 1024;

pub use crate::chunker::fastcdc::Config as FastCdcConfig;

#[derive(Debug, Clone, PartialEq, Eq)]
struct ChunkSignature {
    offset: u64,
    len: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignatureTable {
    chunks: Vec<ChunkSignature>,
    by_hash: HashMap<[u8; 32], Vec<usize>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecipeChunk {
    Literal(Vec<u8>),
    Copy { offset: u64, len: usize },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recipe {
    pub chunks: Vec<RecipeChunk>,
}

pub fn signatures_fastcdc(
    reader: &mut (impl Read + Seek),
    config: FastCdcConfig,
) -> Result<SignatureTable, StrategyError> {
    // Pass 1 finds chunk boundaries. Pass 2 rewinds and hashes each chunk.
    let boundaries = fastcdc_boundaries(reader, config)?;
    rewind(reader, "rewind basis file after FastCDC scan")?;

    let mut chunks = Vec::with_capacity(boundaries.len());
    let mut by_hash = HashMap::with_capacity(boundaries.len());
    let mut buffer = Vec::new();

    for chunk in boundaries {
        read_chunk_bytes(
            reader,
            &mut buffer,
            chunk.length,
            "read FastCDC basis chunk",
        )?;
        let strong = strong_checksum(&buffer);
        by_hash
            .entry(strong)
            .or_insert_with(Vec::new)
            .push(chunks.len());
        chunks.push(ChunkSignature {
            offset: chunk.offset,
            len: chunk.length,
        });
    }

    Ok(SignatureTable { chunks, by_hash })
}

pub fn delta_fastcdc(
    reader: &mut (impl Read + Seek),
    signatures: &SignatureTable,
    config: FastCdcConfig,
) -> Result<Recipe, StrategyError> {
    // The source side mirrors `signatures_fastcdc`: discover chunk boundaries
    // first, then rewind and classify each chunk as copy-or-literal.
    let boundaries = fastcdc_boundaries(reader, config)?;
    rewind(reader, "rewind source file after FastCDC scan")?;

    let mut recipe = Recipe {
        chunks: Vec::with_capacity(boundaries.len()),
    };
    let mut buffer = Vec::new();

    for chunk in boundaries {
        read_chunk_bytes(
            reader,
            &mut buffer,
            chunk.length,
            "read FastCDC source chunk",
        )?;
        let strong = strong_checksum(&buffer);

        if let Some(signature) = find_match(signatures, strong, chunk.length) {
            recipe.chunks.push(RecipeChunk::Copy {
                offset: signature.offset,
                len: signature.len,
            });
        } else {
            push_literal(&mut recipe, &buffer);
        }
    }

    Ok(recipe)
}

pub fn apply(
    recipe: &Recipe,
    basis: &mut (impl Read + Seek),
    writer: &mut impl Write,
) -> Result<(), StrategyError> {
    let mut buffer = [0_u8; READ_BUFFER_SIZE];

    for chunk in &recipe.chunks {
        match chunk {
            RecipeChunk::Literal(bytes) => {
                writer
                    .write_all(bytes)
                    .map_err(|source| StrategyError::Io {
                        operation: "write literal CDC bytes",
                        source,
                    })?;
            }
            RecipeChunk::Copy { offset, len } => {
                basis
                    .seek(SeekFrom::Start(*offset))
                    .map_err(|source| StrategyError::Io {
                        operation: "seek basis file for CDC chunk copy",
                        source,
                    })?;

                let mut remaining = *len;
                while remaining > 0 {
                    let read_len = remaining.min(buffer.len());
                    let read = basis.read(&mut buffer[..read_len]).map_err(|source| {
                        StrategyError::Io {
                            operation: "read referenced CDC basis chunk",
                            source,
                        }
                    })?;

                    if read == 0 {
                        return Err(StrategyError::InvalidBasisSpan {
                            offset: *offset,
                            len: *len,
                        });
                    }

                    writer
                        .write_all(&buffer[..read])
                        .map_err(|source| StrategyError::Io {
                            operation: "write copied CDC bytes",
                            source,
                        })?;
                    remaining -= read;
                }
            }
        }
    }

    Ok(())
}

fn fastcdc_boundaries(
    reader: &mut (impl Read + Seek),
    config: FastCdcConfig,
) -> Result<Vec<Chunk>, StrategyError> {
    Ok(fastcdc::chunk_reader(&mut *reader, config)?)
}

fn rewind(reader: &mut impl Seek, operation: &'static str) -> Result<(), StrategyError> {
    reader
        .seek(SeekFrom::Start(0))
        .map_err(|source| StrategyError::Io { operation, source })?;
    Ok(())
}

fn read_chunk_bytes(
    reader: &mut impl Read,
    buffer: &mut Vec<u8>,
    len: usize,
    operation: &'static str,
) -> Result<(), StrategyError> {
    buffer.resize(len, 0);
    reader
        .read_exact(buffer.as_mut_slice())
        .map_err(|source| StrategyError::Io { operation, source })
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

fn push_literal(recipe: &mut Recipe, bytes: &[u8]) {
    if bytes.is_empty() {
        return;
    }

    match recipe.chunks.last_mut() {
        Some(RecipeChunk::Literal(existing)) => existing.extend_from_slice(bytes),
        _ => recipe.chunks.push(RecipeChunk::Literal(bytes.to_vec())),
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::{apply, delta_fastcdc, signatures_fastcdc, FastCdcConfig, RecipeChunk};

    fn patterned_bytes(len: usize) -> Vec<u8> {
        (0..len)
            .map(|index| ((index * 31 + index / 97) % 251) as u8)
            .collect()
    }

    #[test]
    fn identical_files_turn_into_copy_only_recipes() {
        let basis_bytes = patterned_bytes(256 * 1024);
        let config = FastCdcConfig::default();

        let signatures = signatures_fastcdc(&mut Cursor::new(&basis_bytes), config).unwrap();
        let recipe = delta_fastcdc(&mut Cursor::new(&basis_bytes), &signatures, config).unwrap();

        assert!(!recipe.chunks.is_empty());
        assert!(recipe
            .chunks
            .iter()
            .all(|chunk| matches!(chunk, RecipeChunk::Copy { .. })));

        let mut rebuilt = Vec::new();
        apply(&recipe, &mut Cursor::new(&basis_bytes), &mut rebuilt).unwrap();
        assert_eq!(rebuilt, basis_bytes);
    }

    #[test]
    fn shifted_inserts_preserve_copy_reuse_and_rebuild_the_source() {
        let basis_bytes = patterned_bytes(320 * 1024);
        let mut source_bytes = basis_bytes[..128 * 1024].to_vec();
        source_bytes.extend(std::iter::repeat_n(b'!', 4 * 1024));
        source_bytes.extend_from_slice(&basis_bytes[128 * 1024..]);
        let config = FastCdcConfig::default();

        let signatures = signatures_fastcdc(&mut Cursor::new(&basis_bytes), config).unwrap();
        let recipe = delta_fastcdc(&mut Cursor::new(&source_bytes), &signatures, config).unwrap();

        assert!(recipe
            .chunks
            .iter()
            .any(|chunk| matches!(chunk, RecipeChunk::Copy { .. })));
        assert!(recipe
            .chunks
            .iter()
            .any(|chunk| matches!(chunk, RecipeChunk::Literal(_))));

        let mut rebuilt = Vec::new();
        apply(&recipe, &mut Cursor::new(&basis_bytes), &mut rebuilt).unwrap();
        assert_eq!(rebuilt, source_bytes);
    }

    #[test]
    fn equal_size_overwrites_still_rebuild_the_source() {
        let basis_bytes = patterned_bytes(192 * 1024);
        let mut source_bytes = basis_bytes.clone();
        source_bytes[80 * 1024..84 * 1024].fill(b'?');
        let config = FastCdcConfig::default();

        let signatures = signatures_fastcdc(&mut Cursor::new(&basis_bytes), config).unwrap();
        let recipe = delta_fastcdc(&mut Cursor::new(&source_bytes), &signatures, config).unwrap();

        let mut rebuilt = Vec::new();
        apply(&recipe, &mut Cursor::new(&basis_bytes), &mut rebuilt).unwrap();
        assert_eq!(rebuilt, source_bytes);
    }
}
