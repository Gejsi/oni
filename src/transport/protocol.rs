//! Wire-level protocol definitions layered on top of framed stdio I/O.
//!
//! Keep the split between this module and `framing` strict:
//! - `framing` only knows how to read and write `[length][payload]`
//! - this module defines what the bytes inside the preface and frame payloads mean
//!
//! The wire shape is intentionally small:
//!
//! 1. Before any framed traffic starts, each side exchanges one fixed-size
//!    preface on the raw stream.
//! 2. After that, every frame payload starts with one fixed-size header.
//!
//! We do not define the full message catalog here yet. The point of this file
//! is to lock in the boring envelope that later messages will live inside.

use std::fmt;

use crate::error::ProtocolError;

pub type ProtocolVersion = u16;
pub type MessageKind = u16;
pub type StreamId = u32;

pub const INITIAL_PROTOCOL_VERSION: ProtocolVersion = 1;
pub const CURRENT_PROTOCOL_VERSION: ProtocolVersion = INITIAL_PROTOCOL_VERSION;

/// Raw bytes sent before the framed transport starts.
///
/// This magic is the first thing the peer must write. If the remote shell
/// prints garbage, the wrong binary runs, or stderr/stdout get mixed up, this
/// lets us fail immediately instead of mis-parsing junk as a frame length.
pub const PREFACE_MAGIC: [u8; 4] = *b"ONI\0";
pub const PREFACE_LEN: usize = 12;

/// Every framed payload begins with this fixed-size header.
///
/// `kind` tells the receiver what body layout follows.
/// `flags` are reserved per message kind. The first protocol version can keep
/// them zero everywhere.
/// `stream_id` makes pipelining/multiplexing possible later without redesigning
/// the envelope. The first implementation can still use only one active stream.
pub const FRAME_HEADER_LEN: usize = 8;

/// Inclusive supported-version range for one peer.
#[derive(Debug, PartialEq, Eq)]
pub struct VersionRange {
    min: ProtocolVersion,
    max: ProtocolVersion,
}

impl VersionRange {
    pub fn new(min: ProtocolVersion, max: ProtocolVersion) -> Result<Self, ProtocolError> {
        if min > max {
            return Err(ProtocolError::InvalidVersionRange { min, max });
        }

        Ok(Self { min, max })
    }

    pub const fn exact(version: ProtocolVersion) -> Self {
        Self {
            min: version,
            max: version,
        }
    }

    pub const fn min(&self) -> ProtocolVersion {
        self.min
    }

    pub const fn max(&self) -> ProtocolVersion {
        self.max
    }

    pub fn highest_shared(&self, other: &Self) -> Option<ProtocolVersion> {
        let min = self.min.max(other.min);
        let max = self.max.min(other.max);

        if min <= max {
            Some(max)
        } else {
            None
        }
    }
}

impl fmt::Display for VersionRange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}..={}", self.min, self.max)
    }
}

/// Fixed-size raw connection preface.
///
/// Wire layout, little-endian:
/// - magic: 4 bytes
/// - min-version: u16
/// - max-version: u16
/// - max-frame-bytes: u32
#[derive(Debug, PartialEq, Eq)]
pub struct Preface {
    pub versions: VersionRange,
    pub max_frame_bytes: u32,
}

impl Preface {
    pub const DEFAULT_MAX_FRAME_BYTES: u32 = 8 * 1024 * 1024;

    pub fn new(versions: VersionRange, max_frame_bytes: u32) -> Result<Self, ProtocolError> {
        if max_frame_bytes == 0 {
            return Err(ProtocolError::InvalidLimit {
                field: "max-frame-bytes",
                value: max_frame_bytes,
            });
        }

        Ok(Self {
            versions,
            max_frame_bytes,
        })
    }

    pub fn for_current(max_frame_bytes: u32) -> Result<Self, ProtocolError> {
        Self::new(
            VersionRange::exact(CURRENT_PROTOCOL_VERSION),
            max_frame_bytes,
        )
    }

    pub fn negotiate(local: &Self, remote: &Self) -> Result<NegotiatedSession, ProtocolError> {
        let version = local.versions.highest_shared(&remote.versions).ok_or(
            ProtocolError::NoSharedVersion {
                local: VersionRange::new(local.versions.min(), local.versions.max())
                    .expect("stored version range is always valid"),
                remote: VersionRange::new(remote.versions.min(), remote.versions.max())
                    .expect("stored version range is always valid"),
            },
        )?;

        Ok(NegotiatedSession {
            version,
            max_frame_bytes: local.max_frame_bytes.min(remote.max_frame_bytes),
        })
    }

    pub fn encode(&self) -> [u8; PREFACE_LEN] {
        let mut encoded = [0_u8; PREFACE_LEN];

        encoded[0..4].copy_from_slice(&PREFACE_MAGIC);
        encoded[4..6].copy_from_slice(&self.versions.min().to_le_bytes());
        encoded[6..8].copy_from_slice(&self.versions.max().to_le_bytes());
        encoded[8..12].copy_from_slice(&self.max_frame_bytes.to_le_bytes());

        encoded
    }

    pub fn decode(encoded: &[u8]) -> Result<Self, ProtocolError> {
        let mut cursor = Cursor::new("Preface", encoded);

        let magic_bytes = cursor.read_bytes("magic", 4)?;
        let magic = [
            magic_bytes[0],
            magic_bytes[1],
            magic_bytes[2],
            magic_bytes[3],
        ];
        if magic != PREFACE_MAGIC {
            return Err(ProtocolError::InvalidMagic { found: magic });
        }

        let min_version = cursor.read_u16("min-version")?;
        let max_version = cursor.read_u16("max-version")?;
        let max_frame_bytes = cursor.read_u32("max-frame-bytes")?;

        Self::new(
            VersionRange::new(min_version, max_version)?,
            max_frame_bytes,
        )
    }
}

impl Default for Preface {
    fn default() -> Self {
        Self {
            versions: VersionRange::exact(CURRENT_PROTOCOL_VERSION),
            max_frame_bytes: Self::DEFAULT_MAX_FRAME_BYTES,
        }
    }
}

/// Agreed session limits after both sides exchange their prefaces.
#[derive(Debug, PartialEq, Eq)]
pub struct NegotiatedSession {
    pub version: ProtocolVersion,
    pub max_frame_bytes: u32,
}

/// Fixed-size header stored at the start of every framed payload.
#[derive(Debug, PartialEq, Eq)]
pub struct FrameHeader {
    pub kind: MessageKind,
    pub flags: u16,
    pub stream_id: StreamId,
}

impl FrameHeader {
    pub const fn new(kind: MessageKind, flags: u16, stream_id: StreamId) -> Self {
        Self {
            kind,
            flags,
            stream_id,
        }
    }

    pub fn encode(&self) -> [u8; FRAME_HEADER_LEN] {
        let mut encoded = [0_u8; FRAME_HEADER_LEN];

        encoded[0..2].copy_from_slice(&self.kind.to_le_bytes());
        encoded[2..4].copy_from_slice(&self.flags.to_le_bytes());
        encoded[4..8].copy_from_slice(&self.stream_id.to_le_bytes());

        encoded
    }

    pub fn decode(encoded: &[u8]) -> Result<Self, ProtocolError> {
        let mut cursor = Cursor::new("FrameHeader", encoded);

        Ok(Self {
            kind: cursor.read_u16("kind")?,
            flags: cursor.read_u16("flags")?,
            stream_id: cursor.read_u32("stream-id")?,
        })
    }
}

struct Cursor<'a> {
    message: &'static str,
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Cursor<'a> {
    fn new(message: &'static str, bytes: &'a [u8]) -> Self {
        Self {
            message,
            bytes,
            offset: 0,
        }
    }

    fn read_u16(&mut self, field: &'static str) -> Result<u16, ProtocolError> {
        let bytes = self.read_bytes(field, 2)?;
        Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
    }

    fn read_u32(&mut self, field: &'static str) -> Result<u32, ProtocolError> {
        let bytes = self.read_bytes(field, 4)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn read_bytes(&mut self, field: &'static str, len: usize) -> Result<&'a [u8], ProtocolError> {
        if self.offset + len > self.bytes.len() {
            return Err(ProtocolError::TruncatedMessage {
                message: self.message,
                field,
            });
        }

        let bytes = &self.bytes[self.offset..self.offset + len];
        self.offset += len;
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        FrameHeader, Preface, VersionRange, CURRENT_PROTOCOL_VERSION, FRAME_HEADER_LEN,
        INITIAL_PROTOCOL_VERSION, PREFACE_LEN, PREFACE_MAGIC,
    };
    use crate::error::ProtocolError;

    #[test]
    fn initial_protocol_version_is_current() {
        assert_eq!(INITIAL_PROTOCOL_VERSION, CURRENT_PROTOCOL_VERSION);
    }

    #[test]
    fn rejects_inverted_version_ranges() {
        let error = VersionRange::new(3, 2).unwrap_err();

        assert_eq!(error, ProtocolError::InvalidVersionRange { min: 3, max: 2 });
    }

    #[test]
    fn rejects_zero_frame_limits() {
        let error = Preface::new(VersionRange::exact(1), 0).unwrap_err();

        assert_eq!(
            error,
            ProtocolError::InvalidLimit {
                field: "max-frame-bytes",
                value: 0,
            }
        );
    }

    #[test]
    fn preface_has_a_stable_fixed_size() {
        assert_eq!(PREFACE_LEN, 12);
    }

    #[test]
    fn preface_round_trips() {
        let preface = Preface::new(VersionRange::new(1, 3).unwrap(), 8192).unwrap();

        let encoded = preface.encode();
        let decoded = Preface::decode(&encoded).unwrap();

        assert_eq!(decoded, preface);
    }

    #[test]
    fn preface_starts_with_magic() {
        let preface = Preface::default();
        let encoded = preface.encode();

        assert_eq!(&encoded[0..4], &PREFACE_MAGIC);
    }

    #[test]
    fn rejects_wrong_magic() {
        let mut encoded = Preface::default().encode();
        encoded[0..4].copy_from_slice(b"NOPE");

        let error = Preface::decode(&encoded).unwrap_err();

        assert_eq!(error, ProtocolError::InvalidMagic { found: *b"NOPE" });
    }

    #[test]
    fn negotiates_highest_shared_version_and_smallest_frame_limit() {
        let local = Preface::new(VersionRange::new(1, 3).unwrap(), 8 * 1024 * 1024).unwrap();
        let remote = Preface::new(VersionRange::new(2, 4).unwrap(), 4 * 1024 * 1024).unwrap();

        let negotiated = Preface::negotiate(&local, &remote).unwrap();

        assert_eq!(negotiated.version, 3);
        assert_eq!(negotiated.max_frame_bytes, 4 * 1024 * 1024);
    }

    #[test]
    fn reports_when_no_protocol_version_overlaps() {
        let local = Preface::new(VersionRange::exact(1), 1024).unwrap();
        let remote = Preface::new(VersionRange::exact(2), 1024).unwrap();

        let error = Preface::negotiate(&local, &remote).unwrap_err();

        assert_eq!(
            error,
            ProtocolError::NoSharedVersion {
                local: VersionRange::exact(1),
                remote: VersionRange::exact(2),
            }
        );
    }

    #[test]
    fn rejects_truncated_prefaces() {
        let encoded = &Preface::default().encode()[0..10];
        let error = Preface::decode(encoded).unwrap_err();

        assert_eq!(
            error,
            ProtocolError::TruncatedMessage {
                message: "Preface",
                field: "max-frame-bytes",
            }
        );
    }

    #[test]
    fn frame_header_has_a_stable_fixed_size() {
        assert_eq!(FRAME_HEADER_LEN, 8);
    }

    #[test]
    fn frame_header_round_trips() {
        let header = FrameHeader::new(0x0020, 0, 17);

        let encoded = header.encode();
        let decoded = FrameHeader::decode(&encoded).unwrap();

        assert_eq!(decoded, header);
    }

    #[test]
    fn rejects_truncated_frame_headers() {
        let encoded = &FrameHeader::new(0x0020, 0, 17).encode()[0..6];
        let error = FrameHeader::decode(encoded).unwrap_err();

        assert_eq!(
            error,
            ProtocolError::TruncatedMessage {
                message: "FrameHeader",
                field: "stream-id",
            }
        );
    }
}
