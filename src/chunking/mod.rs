use std::ops::Range;

use crate::error::PlanError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    pub offset: u64,
    pub length: u32,
    pub fingerprint: u64,
}

impl Chunk {
    pub fn byte_range(&self) -> Range<u64> {
        self.offset..(self.offset + u64::from(self.length))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkingConfig {
    pub min_size: u32,
    pub avg_size: u32,
    pub max_size: u32,
}

impl ChunkingConfig {
    pub const fn fastcdc_defaults() -> Self {
        Self {
            min_size: 16 * 1024,
            avg_size: 64 * 1024,
            max_size: 256 * 1024,
        }
    }
}

pub trait Chunker: Send + Sync {
    fn algorithm(&self) -> &'static str;
    fn config(&self) -> ChunkingConfig;
    fn chunk_file(&self, _path: &std::path::Path) -> Result<Vec<Chunk>, PlanError>;
}

pub struct FastCdcChunker {
    config: ChunkingConfig,
}

impl FastCdcChunker {
    pub fn new(config: ChunkingConfig) -> Self {
        Self { config }
    }
}

impl Default for FastCdcChunker {
    fn default() -> Self {
        Self::new(ChunkingConfig::fastcdc_defaults())
    }
}

impl Chunker for FastCdcChunker {
    fn algorithm(&self) -> &'static str {
        "fastcdc"
    }

    fn config(&self) -> ChunkingConfig {
        self.config
    }

    fn chunk_file(&self, _path: &std::path::Path) -> Result<Vec<Chunk>, PlanError> {
        Err(PlanError::NotImplemented {
            feature: "FastCDC chunking",
        })
    }
}
