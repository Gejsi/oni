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
    pub weak: u32,
    pub strong: [u8; 32],
    pub block_index: usize,
    pub len: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignatureTable {
    block_size: usize,
    blocks: Vec<BlockSignature>,
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
    Literal(Vec<u8>),
    Copy { block_index: usize, len: usize },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recipe {
    pub block_size: usize,
    pub chunks: Vec<RecipeChunk>,
}

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
    let mut window = fill_window(&mut source, signatures.block_size)?;
    let mut recipe = Recipe {
        block_size: signatures.block_size,
        chunks: Vec::new(),
    };
    let mut literal = Vec::new();
    let mut previous_weak = None;

    if window.len() < signatures.block_size {
        if !window.is_empty() {
            recipe.chunks.push(RecipeChunk::Literal(window));
        }

        return Ok(recipe);
    }

    loop {
        let current_weak = if let Some(weak) = previous_weak {
            weak
        } else {
            weak_checksum(&window)
        };

        if let Some(block) = find_match(signatures, &window, current_weak) {
            if !literal.is_empty() {
                recipe
                    .chunks
                    .push(RecipeChunk::Literal(std::mem::take(&mut literal)));
            }

            recipe.chunks.push(RecipeChunk::Copy {
                block_index: block.block_index,
                len: block.len,
            });

            window = fill_window(&mut source, signatures.block_size)?;
            if window.len() < signatures.block_size {
                if !window.is_empty() {
                    literal.extend_from_slice(&window);
                }
                break;
            }

            previous_weak = None;
            continue;
        }

        let outgoing = window.remove(0);
        literal.push(outgoing);

        let Some(incoming) = source.next_byte()? else {
            literal.extend_from_slice(&window);
            break;
        };

        window.push(incoming);
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
    window: &[u8],
    weak: u32,
) -> Option<&'a BlockSignature> {
    let candidates = signatures.by_weak.get(&weak)?;
    let strong = strong_checksum(window);

    candidates.iter().find_map(|index| {
        let candidate = &signatures.blocks[*index];
        (candidate.weak == weak && candidate.strong == strong).then_some(candidate)
    })
}

fn fill_window(
    source: &mut ByteStream<'_, impl Read>,
    block_size: usize,
) -> Result<Vec<u8>, StrategyError> {
    let mut window = Vec::with_capacity(block_size);

    while window.len() < block_size {
        let Some(byte) = source.next_byte()? else {
            break;
        };
        window.push(byte);
    }

    Ok(window)
}

fn read_block(
    reader: &mut impl Read,
    buffer: &mut [u8],
    operation: &'static str,
) -> Result<usize, StrategyError> {
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

fn weak_checksum(data: &[u8]) -> u32 {
    let mut a = 0_u32;
    let mut b = 0_u32;

    for (index, byte) in data.iter().enumerate() {
        a = (a + *byte as u32).rem_euclid(ROLL_MOD);
        b = (b + (data.len() as u32 - index as u32) * *byte as u32).rem_euclid(ROLL_MOD);
    }

    (b << 16) | a
}

fn roll_weak_checksum(previous: u32, outgoing: u8, incoming: u8, block_size: usize) -> u32 {
    let a = previous & 0xFFFF;
    let b = (previous >> 16) & 0xFFFF;

    let next_a = ((a + incoming as u32).rem_euclid(ROLL_MOD) + ROLL_MOD
        - (outgoing as u32).rem_euclid(ROLL_MOD))
    .rem_euclid(ROLL_MOD);
    let next_b = ((b + next_a).rem_euclid(ROLL_MOD) + ROLL_MOD
        - (block_size as u32 * outgoing as u32).rem_euclid(ROLL_MOD))
    .rem_euclid(ROLL_MOD);

    (next_b << 16) | next_a
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
