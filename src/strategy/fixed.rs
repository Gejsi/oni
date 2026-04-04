//! Fixed-size rsync-style delta primitives.
//!
//! This module stays transport-agnostic on purpose:
//! - build signatures for an existing basis file
//! - generate a deterministic recipe for the source file
//! - replay that recipe against the basis to reconstruct the source
//!
//! High-level flow:
//! 1. the receiver-side file becomes a `SignatureTable`
//! 2. the sender-side file is scanned with a rolling window of the same size
//! 3. every matching window becomes `RecipeChunk::Copy`
//! 4. every non-matching byte range becomes `RecipeChunk::Literal`
//! 5. `apply` replays the recipe against the basis file to reconstruct the
//!    desired output
//!
//! ASCII view:
//!
//!   basis file --fixed blocks----------> weak+strong signatures
//!   source file --rolling byte scan---> copy/literal recipe
//!   recipe + basis file --------------> rebuilt output
//!
//! This file owns only the algorithmic core:
//! - no transport
//! - no filesystem metadata
//! - no protocol structs
//! - no async orchestration
//! - no partial checksum key truncation

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom, Write};

use blake3::Hasher;

use crate::error::StrategyError;

const KB: usize = 1024;
const MB: usize = 1024 * KB;
const MIN_BLOCK_SIZE: usize = 4 * KB;
const MAX_BLOCK_SIZE: usize = 4 * MB;
const READ_BUFFER_SIZE: usize = 8 * KB;
const ROLL_MOD: u32 = 1 << 16;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockSignature {
    /// Cheap rolling checksum used to filter candidate matches quickly.
    pub weak: u32,
    /// Strong checksum used to confirm that a weak match is a real match.
    pub strong: [u8; 32],
    /// Block index inside the basis file.
    pub block_index: usize,
    /// Real block length. The last basis block may be shorter than `block_size`.
    pub len: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignatureTable {
    /// Shared fixed block size for this basis/source comparison.
    block_size: usize,
    /// Basis blocks in original order.
    blocks: Vec<BlockSignature>,
    /// Weak-checksum index for fast candidate lookup during source scanning.
    ///
    /// Unlike the old `main` code, this keeps the full weak checksum as the
    /// key instead of truncating it to `u16`.
    by_weak: HashMap<u32, Vec<usize>>,
}

impl SignatureTable {
    pub fn block_size(&self) -> usize {
        self.block_size
    }

    pub fn blocks(&self) -> &[BlockSignature] {
        &self.blocks
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecipeChunk {
    /// Raw bytes that do not match any basis block.
    Literal(Vec<u8>),
    /// Copy `len` bytes starting from `block_index * block_size` in the basis.
    Copy { block_index: usize, len: usize },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recipe {
    /// The fixed block size used for both signatures and copy references.
    pub block_size: usize,
    /// Ordered stream of literals and copy instructions.
    pub chunks: Vec<RecipeChunk>,
}

/// Pick one shared fixed block size for both files.
///
/// Like the old code, this uses a square-root heuristic clamped to a safe range.
/// The important simplification is that the caller already knows both file
/// lengths, so this function only chooses a size; it does not reach into I/O or
/// transport code.
pub fn choose_block_size(source_len: u64, destination_len: u64) -> usize {
    let smaller = source_len.min(destination_len);

    if smaller == 0 {
        return MIN_BLOCK_SIZE;
    }

    ((smaller as f64).sqrt().round() as usize).clamp(MIN_BLOCK_SIZE, MAX_BLOCK_SIZE)
}

pub fn signatures(
    reader: &mut impl Read,
    block_size: usize,
) -> Result<SignatureTable, StrategyError> {
    // Read the basis file in fixed-size blocks and index each full block by its
    // weak checksum. The last short block is still recorded in `blocks`, but it
    // is not inserted into `by_weak` because the rolling scanner only compares
    // windows of exactly `block_size`.
    let mut blocks = Vec::new();
    let mut by_weak = HashMap::new();
    let mut buffer = vec![0_u8; block_size];
    let mut block_index = 0;

    loop {
        let read = read_block(reader, &mut buffer, "read basis block")?;
        if read == 0 {
            break;
        }

        let block = &buffer[..read];
        let signature = BlockSignature {
            weak: weak_checksum(block),
            strong: strong_checksum(block),
            block_index,
            len: read,
        };

        if read == block_size {
            by_weak
                .entry(signature.weak)
                .or_insert_with(Vec::new)
                .push(blocks.len());
        }

        blocks.push(signature);
        block_index += 1;

        if read < block_size {
            break;
        }
    }

    Ok(SignatureTable {
        block_size,
        blocks,
        by_weak,
    })
}

pub fn delta(reader: &mut impl Read, signatures: &SignatureTable) -> Result<Recipe, StrategyError> {
    let mut source = ByteStream::new(reader, "read source byte");
    let mut window = Window::new(signatures.block_size);
    window.fill_from(&mut source)?;
    let mut recipe = Recipe {
        block_size: signatures.block_size,
        chunks: Vec::new(),
    };
    let mut literal = Vec::new();
    let mut previous_weak = None;

    // If the source is shorter than one full block, nothing can match by block
    // reference. The entire source becomes one literal.
    if window.len() < signatures.block_size {
        if !window.is_empty() {
            recipe
                .chunks
                .push(RecipeChunk::Literal(window.iter().collect()));
        }

        return Ok(recipe);
    }

    loop {
        // On the first position we compute the weak checksum from scratch.
        // After that we reuse the previous checksum and roll it forward by one
        // byte on each miss.
        let current_weak = if let Some(weak) = previous_weak {
            weak
        } else {
            weak_checksum_window(&window)
        };

        if let Some(block) = find_match(signatures, &window, current_weak) {
            // Flush any bytes we accumulated while no basis match existed.
            if !literal.is_empty() {
                recipe
                    .chunks
                    .push(RecipeChunk::Literal(std::mem::take(&mut literal)));
            }

            // A confirmed block match becomes a copy instruction instead of
            // resending those bytes literally.
            recipe.chunks.push(RecipeChunk::Copy {
                block_index: block.block_index,
                len: block.len,
            });

            // After a full-block match we advance by one whole block, refill
            // the window from the source stream, and reset the rolling
            // checksum because the next comparison starts from a new position.
            window.fill_from(&mut source)?;
            if window.len() < signatures.block_size {
                if !window.is_empty() {
                    literal.extend(window.iter());
                }
                break;
            }

            previous_weak = None;
            continue;
        }

        // Sliding one byte at a time is the hot path. Keep it O(1) instead of
        // shifting the whole window left on every miss. The ring buffer keeps
        // one allocation for the whole scan and reuses those slots.
        let Some(incoming) = source.next_byte()? else {
            // No more source bytes means the remaining window can never match a
            // future block. It becomes trailing literal data.
            literal.extend(window.iter());
            break;
        };
        let Some(outgoing) = window.slide(incoming) else {
            break;
        };
        literal.push(outgoing);
        previous_weak = Some(roll_weak_checksum(
            current_weak,
            outgoing,
            incoming,
            signatures.block_size,
        ));
    }

    if !literal.is_empty() {
        recipe.chunks.push(RecipeChunk::Literal(literal));
    }

    Ok(recipe)
}

pub fn apply(
    recipe: &Recipe,
    basis: &mut (impl Read + Seek),
    writer: &mut impl Write,
) -> Result<(), StrategyError> {
    // Rebuild the desired output by alternating:
    // - raw literal writes
    // - block copies from the basis file
    let mut buffer = [0_u8; READ_BUFFER_SIZE];

    for chunk in &recipe.chunks {
        match chunk {
            RecipeChunk::Literal(bytes) => {
                writer
                    .write_all(bytes)
                    .map_err(|source| StrategyError::Io {
                        operation: "write literal delta bytes",
                        source,
                    })?;
            }
            RecipeChunk::Copy { block_index, len } => {
                let start = (*block_index as u64)
                    .checked_mul(recipe.block_size as u64)
                    .ok_or(StrategyError::InvalidBlockReference {
                        block_index: *block_index,
                    })?;
                basis
                    .seek(SeekFrom::Start(start))
                    .map_err(|source| StrategyError::Io {
                        operation: "seek basis file for block copy",
                        source,
                    })?;

                let mut remaining = *len;
                while remaining > 0 {
                    let read_len = remaining.min(buffer.len());
                    let read = basis.read(&mut buffer[..read_len]).map_err(|source| {
                        StrategyError::Io {
                            operation: "read referenced basis block",
                            source,
                        }
                    })?;

                    if read == 0 {
                        return Err(StrategyError::InvalidBlockReference {
                            block_index: *block_index,
                        });
                    }

                    writer
                        .write_all(&buffer[..read])
                        .map_err(|source| StrategyError::Io {
                            operation: "write copied delta bytes",
                            source,
                        })?;
                    remaining -= read;
                }
            }
        }
    }

    Ok(())
}

fn find_match<'a>(
    signatures: &'a SignatureTable,
    window: &Window,
    weak: u32,
) -> Option<&'a BlockSignature> {
    // The weak checksum narrows the search to a tiny candidate set.
    // Only then do we pay for the strong hash over the current window.
    let candidates = signatures.by_weak.get(&weak)?;
    let strong = strong_checksum_window(window);

    candidates.iter().find_map(|index| {
        let candidate = &signatures.blocks[*index];
        (candidate.weak == weak && candidate.strong == strong).then_some(candidate)
    })
}

fn read_block(
    reader: &mut impl Read,
    buffer: &mut [u8],
    operation: &'static str,
) -> Result<usize, StrategyError> {
    // `Read::read` may return short reads even before EOF. Keep reading until
    // either the block buffer is full or the input is exhausted.
    let mut offset = 0;

    while offset < buffer.len() {
        let read = reader
            .read(&mut buffer[offset..])
            .map_err(|source| StrategyError::Io { operation, source })?;

        if read == 0 {
            break;
        }

        offset += read;
    }

    Ok(offset)
}

fn strong_checksum(data: &[u8]) -> [u8; 32] {
    let hash = Hasher::new().update(data).finalize();
    *hash.as_bytes()
}

fn strong_checksum_window(window: &Window) -> [u8; 32] {
    // The window is a ring buffer, so the logical byte order may be split into
    // two slices. Hash both slices in order instead of building a temporary
    // contiguous buffer.
    let (head, tail) = window.as_slices();
    let mut hasher = Hasher::new();
    hasher.update(head);
    hasher.update(tail);
    *hasher.finalize().as_bytes()
}

fn weak_checksum(data: &[u8]) -> u32 {
    weak_checksum_iter(data.iter().copied(), data.len())
}

fn weak_checksum_window(window: &Window) -> u32 {
    weak_checksum_iter(window.iter(), window.len())
}

fn weak_checksum_iter(bytes: impl Iterator<Item = u8>, len: usize) -> u32 {
    // This is the rsync-style rolling checksum:
    // - `a` is the sum of bytes
    // - `b` is the weighted sum of bytes
    //
    // Packing them into one `u32` keeps the update function cheap.
    let mut a = 0_u32;
    let mut b = 0_u32;

    for (index, byte) in bytes.enumerate() {
        a = (a + byte as u32).rem_euclid(ROLL_MOD);
        b = (b + (len as u32 - index as u32) * byte as u32).rem_euclid(ROLL_MOD);
    }

    (b << 16) | a
}

fn roll_weak_checksum(previous: u32, outgoing: u8, incoming: u8, block_size: usize) -> u32 {
    let a = previous & 0xFFFF;
    let b = (previous >> 16) & 0xFFFF;

    // Sliding the window by one byte means:
    // - remove the old leading byte
    // - add the new trailing byte
    // - update the weighted sum accordingly
    //
    // The modular arithmetic keeps subtraction well-defined without negative
    // intermediate values.
    let next_a = ((a + incoming as u32).rem_euclid(ROLL_MOD) + ROLL_MOD
        - (outgoing as u32).rem_euclid(ROLL_MOD))
    .rem_euclid(ROLL_MOD);
    let next_b = ((b + next_a).rem_euclid(ROLL_MOD) + ROLL_MOD
        - (block_size as u32 * outgoing as u32).rem_euclid(ROLL_MOD))
    .rem_euclid(ROLL_MOD);

    (next_b << 16) | next_a
}

/// Fixed-size sliding window backed by one reusable heap allocation.
///
/// `bytes` stores the raw allocation. `head` points at the logical first byte
/// in the current window, and `len` tracks how many bytes are valid. During the
/// hot path the window is full, so sliding by one byte simply overwrites the
/// old head slot with the new trailing byte and advances `head`.
struct Window {
    bytes: Vec<u8>,
    head: usize,
    len: usize,
}

impl Window {
    fn new(size: usize) -> Self {
        Self {
            bytes: vec![0_u8; size],
            head: 0,
            len: 0,
        }
    }

    fn len(&self) -> usize {
        self.len
    }

    fn is_empty(&self) -> bool {
        self.len == 0
    }

    fn fill_from(&mut self, source: &mut ByteStream<'_, impl Read>) -> Result<(), StrategyError> {
        self.head = 0;
        self.len = 0;

        while self.len < self.bytes.len() {
            let Some(byte) = source.next_byte()? else {
                break;
            };
            self.bytes[self.len] = byte;
            self.len += 1;
        }

        Ok(())
    }

    fn slide(&mut self, incoming: u8) -> Option<u8> {
        if self.len != self.bytes.len() || self.is_empty() {
            return None;
        }

        let outgoing = self.bytes[self.head];
        self.bytes[self.head] = incoming;
        self.head = (self.head + 1) % self.bytes.len();
        Some(outgoing)
    }

    fn iter(&self) -> impl Iterator<Item = u8> + '_ {
        let (head, tail) = self.as_slices();
        head.iter().chain(tail.iter()).copied()
    }

    fn as_slices(&self) -> (&[u8], &[u8]) {
        if self.len == 0 {
            return (&[], &[]);
        }

        let first_len = (self.bytes.len() - self.head).min(self.len);
        let (left, right) = self.bytes.split_at(self.head);
        let first = &right[..first_len];
        let second = &left[..self.len - first_len];
        (first, second)
    }
}

struct ByteStream<'a, R> {
    reader: &'a mut R,
    operation: &'static str,
    buffer: [u8; READ_BUFFER_SIZE],
    start: usize,
    end: usize,
}

impl<'a, R: Read> ByteStream<'a, R> {
    fn new(reader: &'a mut R, operation: &'static str) -> Self {
        Self {
            reader,
            operation,
            buffer: [0_u8; READ_BUFFER_SIZE],
            start: 0,
            end: 0,
        }
    }

    fn next_byte(&mut self) -> Result<Option<u8>, StrategyError> {
        // Refill in coarse chunks, then hand bytes out one by one. That keeps
        // the delta scanner streaming without making a syscall per byte.
        if self.start == self.end {
            self.end = self
                .reader
                .read(&mut self.buffer)
                .map_err(|source| StrategyError::Io {
                    operation: self.operation,
                    source,
                })?;
            self.start = 0;

            if self.end == 0 {
                return Ok(None);
            }
        }

        let byte = self.buffer[self.start];
        self.start += 1;
        Ok(Some(byte))
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::{apply, choose_block_size, delta, roll_weak_checksum, signatures, RecipeChunk};

    #[test]
    fn rolling_checksum_matches_full_recomputation() {
        let original = b"abcd";
        let rolled = roll_weak_checksum(super::weak_checksum(original), b'a', b'e', original.len());

        assert_eq!(rolled, super::weak_checksum(b"bcde"));
    }

    #[test]
    fn block_size_is_clamped_to_the_configured_range() {
        assert_eq!(choose_block_size(0, 0), 4 * 1024);
        assert_eq!(choose_block_size(16, 16), 4 * 1024);
        assert_eq!(choose_block_size(u64::MAX, u64::MAX), 4 * 1024 * 1024);
    }

    #[test]
    fn identical_inputs_produce_copy_chunks() {
        let basis = b"abcdefgh".to_vec();
        let mut signature_reader = Cursor::new(&basis);
        let signatures = signatures(&mut signature_reader, 4).unwrap();
        let mut source_reader = Cursor::new(&basis);

        let recipe = delta(&mut source_reader, &signatures).unwrap();

        assert_eq!(
            recipe.chunks,
            vec![
                RecipeChunk::Copy {
                    block_index: 0,
                    len: 4,
                },
                RecipeChunk::Copy {
                    block_index: 1,
                    len: 4,
                },
            ]
        );
    }

    #[test]
    fn inserted_prefix_and_suffix_keep_middle_copies() {
        let basis = b"abcd1234wxyz".to_vec();
        let source = b"zzabcd1234wxyzyy".to_vec();
        let mut signature_reader = Cursor::new(&basis);
        let signatures = signatures(&mut signature_reader, 4).unwrap();
        let mut source_reader = Cursor::new(&source);

        let recipe = delta(&mut source_reader, &signatures).unwrap();
        let mut reconstructed = Vec::new();
        let mut basis_reader = Cursor::new(&basis);
        apply(&recipe, &mut basis_reader, &mut reconstructed).unwrap();

        assert_eq!(reconstructed, source);
        assert_eq!(
            recipe.chunks,
            vec![
                RecipeChunk::Literal(b"zz".to_vec()),
                RecipeChunk::Copy {
                    block_index: 0,
                    len: 4,
                },
                RecipeChunk::Copy {
                    block_index: 1,
                    len: 4,
                },
                RecipeChunk::Copy {
                    block_index: 2,
                    len: 4,
                },
                RecipeChunk::Literal(b"yy".to_vec()),
            ]
        );
    }

    #[test]
    fn equal_size_changed_tail_becomes_literal_data() {
        let basis = b"abcdwxyz".to_vec();
        let source = b"abcd1234".to_vec();
        let mut signature_reader = Cursor::new(&basis);
        let signatures = signatures(&mut signature_reader, 4).unwrap();
        let mut source_reader = Cursor::new(&source);

        let recipe = delta(&mut source_reader, &signatures).unwrap();
        let mut reconstructed = Vec::new();
        let mut basis_reader = Cursor::new(&basis);
        apply(&recipe, &mut basis_reader, &mut reconstructed).unwrap();

        assert_eq!(reconstructed, source);
        assert_eq!(
            recipe.chunks,
            vec![
                RecipeChunk::Copy {
                    block_index: 0,
                    len: 4,
                },
                RecipeChunk::Literal(b"1234".to_vec()),
            ]
        );
    }

    #[test]
    fn short_inputs_fall_back_to_one_literal_chunk() {
        let basis = b"abcdefgh".to_vec();
        let source = b"oni".to_vec();
        let mut signature_reader = Cursor::new(&basis);
        let signatures = signatures(&mut signature_reader, 4).unwrap();
        let mut source_reader = Cursor::new(&source);

        let recipe = delta(&mut source_reader, &signatures).unwrap();

        assert_eq!(recipe.chunks, vec![RecipeChunk::Literal(source)]);
    }
}
