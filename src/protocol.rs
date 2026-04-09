//! Internal stdio helper protocol.
//!
//! This module defines versioning, capabilities, negotiated limits,
//! and message encoding without pulling transport or
//! sync execution concerns into the wire schema.

use std::collections::BTreeSet;
use std::fmt;

use crate::error::ProtocolError;

pub const INITIAL_PROTOCOL_VERSION: ProtocolVersion = ProtocolVersion::new(1);
pub const CURRENT_PROTOCOL_VERSION: ProtocolVersion = INITIAL_PROTOCOL_VERSION;

/// Wire-protocol versions are negotiated independently from the Oni package
/// version. This keeps compatibility checks small and explicit.
#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProtocolVersion(u16);

impl ProtocolVersion {
    pub const fn new(value: u16) -> Self {
        Self(value)
    }

    pub const fn as_u16(self) -> u16 {
        self.0
    }
}

impl fmt::Display for ProtocolVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A peer advertises a supported inclusive version range in its `Hello`.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct VersionRange {
    min: ProtocolVersion,
    max: ProtocolVersion,
}

impl VersionRange {
    /// Construct an inclusive supported-version range.
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

    pub const fn min(self) -> ProtocolVersion {
        self.min
    }

    pub const fn max(self) -> ProtocolVersion {
        self.max
    }

    pub fn highest_shared(self, other: Self) -> Option<ProtocolVersion> {
        // Sessions always choose the highest mutually supported protocol
        // version so new peers can still talk to older ones when their ranges
        // overlap.
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

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum PeerRole {
    Coordinator,
    Helper,
}

impl PeerRole {
    fn from_wire(value: u8) -> Result<Self, ProtocolError> {
        match value {
            1 => Ok(Self::Coordinator),
            2 => Ok(Self::Helper),
            _ => Err(ProtocolError::UnknownPeerRole { value }),
        }
    }

    fn wire_id(self) -> u8 {
        match self {
            Self::Coordinator => 1,
            Self::Helper => 2,
        }
    }
}

impl fmt::Display for PeerRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Coordinator => f.write_str("coordinator"),
            Self::Helper => f.write_str("helper"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImplementationInfo {
    pub name: String,
    pub version: String,
}

impl ImplementationInfo {
    pub fn new(name: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            version: version.into(),
        }
    }
}

impl fmt::Display for ImplementationInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.name, self.version)
    }
}

/// Small numeric identifiers avoid string parsing in the handshake and keep
/// capability negotiation explicit.
#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u16)]
pub enum Capability {
    WholeFileTransfer = 0x0001,
    CdcDeltaTransfer,
    FastCdcChunker = 0x0100,
    SeqCdcChunker,
    Symlink = 0x0300,
    Xattr,
    Acl,
    Compression = 0x0400,
    Resume,
    StructuredStats = 0x0500,
}

impl Capability {
    pub const fn id(self) -> u16 {
        self as u16
    }

    fn from_id(id: u16) -> Result<Self, ProtocolError> {
        match id {
            0x0001 => Ok(Self::WholeFileTransfer),
            0x0002 => Ok(Self::CdcDeltaTransfer),
            0x0100 => Ok(Self::FastCdcChunker),
            0x0101 => Ok(Self::SeqCdcChunker),
            0x0300 => Ok(Self::Symlink),
            0x0301 => Ok(Self::Xattr),
            0x0302 => Ok(Self::Acl),
            0x0400 => Ok(Self::Compression),
            0x0401 => Ok(Self::Resume),
            0x0500 => Ok(Self::StructuredStats),
            _ => Err(ProtocolError::UnknownCapabilityId { id }),
        }
    }
}

impl fmt::Display for Capability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WholeFileTransfer => f.write_str("whole-file-transfer"),
            Self::CdcDeltaTransfer => f.write_str("cdc-delta-transfer"),
            Self::FastCdcChunker => f.write_str("fastcdc-chunker"),
            Self::SeqCdcChunker => f.write_str("seqcdc-chunker"),
            Self::Symlink => f.write_str("symlink"),
            Self::Xattr => f.write_str("xattr"),
            Self::Acl => f.write_str("acl"),
            Self::Compression => f.write_str("compression"),
            Self::Resume => f.write_str("resume"),
            Self::StructuredStats => f.write_str("structured-stats"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CapabilitySet {
    inner: BTreeSet<Capability>,
}

impl CapabilitySet {
    pub fn new(capabilities: impl IntoIterator<Item = Capability>) -> Self {
        Self {
            inner: capabilities.into_iter().collect(),
        }
    }

    pub fn contains(&self, capability: Capability) -> bool {
        self.inner.contains(&capability)
    }

    pub fn intersection(&self, other: &Self) -> Self {
        Self::new(self.inner.intersection(&other.inner).copied())
    }

    pub fn len(&self) -> usize {
        self.inner.len()
    }

    pub fn iter(&self) -> impl Iterator<Item = Capability> + '_ {
        self.inner.iter().copied()
    }
}

impl<const N: usize> From<[Capability; N]> for CapabilitySet {
    fn from(value: [Capability; N]) -> Self {
        Self::new(value)
    }
}

impl std::iter::FromIterator<Capability> for CapabilitySet {
    fn from_iter<T: IntoIterator<Item = Capability>>(iter: T) -> Self {
        Self::new(iter)
    }
}

impl fmt::Display for CapabilitySet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut capabilities = self.iter();

        let Some(first) = capabilities.next() else {
            return f.write_str("(none)");
        };

        write!(f, "{first}")?;

        for capability in capabilities {
            write!(f, ", {capability}")?;
        }

        Ok(())
    }
}

/// These ids are negotiated separately from capabilities so messages can refer
/// to a concrete chunker without stringly typed fields.
#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u16)]
pub enum ChunkerId {
    FastCdc = 1,
    SeqCdc,
}

impl ChunkerId {
    pub const fn id(self) -> u16 {
        self as u16
    }
}

impl fmt::Display for ChunkerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FastCdc => f.write_str("fastcdc"),
            Self::SeqCdc => f.write_str("seqcdc"),
        }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u16)]
pub enum OperationKind {
    CreateFile = 1,
    ReplaceFile,
    UpdateMetadata,
    DeleteFile,
    DeleteDirectory,
    Skip,
}

impl OperationKind {
    pub const fn id(self) -> u16 {
        self as u16
    }
}

impl fmt::Display for OperationKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CreateFile => f.write_str("create-file"),
            Self::ReplaceFile => f.write_str("replace-file"),
            Self::UpdateMetadata => f.write_str("update-metadata"),
            Self::DeleteFile => f.write_str("delete-file"),
            Self::DeleteDirectory => f.write_str("delete-directory"),
            Self::Skip => f.write_str("skip"),
        }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct Limits {
    max_frame_bytes: u32,
    max_chunk_bytes: u32,
}

impl Limits {
    pub const DEFAULT_MAX_FRAME_BYTES: u32 = 8 * 1024 * 1024;
    pub const DEFAULT_MAX_CHUNK_BYTES: u32 = 4 * 1024 * 1024;

    pub fn new(max_frame_bytes: u32, max_chunk_bytes: u32) -> Result<Self, ProtocolError> {
        if max_frame_bytes == 0 {
            return Err(ProtocolError::InvalidLimit {
                field: "max-frame-bytes",
                value: max_frame_bytes,
            });
        }

        if max_chunk_bytes == 0 {
            return Err(ProtocolError::InvalidLimit {
                field: "max-chunk-bytes",
                value: max_chunk_bytes,
            });
        }

        Ok(Self {
            max_frame_bytes,
            max_chunk_bytes,
        })
    }

    pub const fn max_frame_bytes(self) -> u32 {
        self.max_frame_bytes
    }

    pub const fn max_chunk_bytes(self) -> u32 {
        self.max_chunk_bytes
    }

    pub fn negotiate(self, other: Self) -> Self {
        Self {
            max_frame_bytes: self.max_frame_bytes.min(other.max_frame_bytes),
            max_chunk_bytes: self.max_chunk_bytes.min(other.max_chunk_bytes),
        }
    }
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_frame_bytes: Self::DEFAULT_MAX_FRAME_BYTES,
            max_chunk_bytes: Self::DEFAULT_MAX_CHUNK_BYTES,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hello {
    pub role: PeerRole,
    pub implementation: ImplementationInfo,
    pub versions: VersionRange,
    pub capabilities: CapabilitySet,
    pub limits: Limits,
}

impl Hello {
    pub fn new(
        role: PeerRole,
        implementation: ImplementationInfo,
        versions: VersionRange,
        capabilities: CapabilitySet,
        limits: Limits,
    ) -> Self {
        Self {
            role,
            implementation,
            versions,
            capabilities,
            limits,
        }
    }

    pub fn for_current(
        role: PeerRole,
        implementation: ImplementationInfo,
        capabilities: CapabilitySet,
        limits: Limits,
    ) -> Self {
        Self::new(
            role,
            implementation,
            VersionRange::exact(CURRENT_PROTOCOL_VERSION),
            capabilities,
            limits,
        )
    }

    pub fn negotiate(local: &Self, remote: &Self) -> Result<NegotiatedSession, ProtocolError> {
        // A session needs exactly one coordinator and one helper. Reject same-
        // role handshakes before comparing any other fields.
        if local.role == remote.role {
            return Err(ProtocolError::IncompatibleRoles {
                local: local.role,
                remote: remote.role,
            });
        }

        let version = local.versions.highest_shared(remote.versions).ok_or(
            ProtocolError::NoSharedVersion {
                local: local.versions,
                remote: remote.versions,
            },
        )?;

        // Versions gate wire compatibility. Capabilities and limits then refine
        // what this specific session is allowed to do.
        Ok(NegotiatedSession {
            version,
            capabilities: local.capabilities.intersection(&remote.capabilities),
            limits: local.limits.negotiate(remote.limits),
            local_role: local.role,
            remote_role: remote.role,
            local_implementation: local.implementation.clone(),
            remote_implementation: remote.implementation.clone(),
        })
    }

    fn encode_body(&self) -> Result<Vec<u8>, ProtocolError> {
        // The wire layout is intentionally simple and fixed-width where
        // possible. That keeps decoding small and avoids schema machinery.
        let mut encoded = Vec::new();

        encoded.push(self.role.wire_id());
        encoded.extend_from_slice(&self.versions.min().as_u16().to_be_bytes());
        encoded.extend_from_slice(&self.versions.max().as_u16().to_be_bytes());
        encoded.extend_from_slice(&self.limits.max_frame_bytes().to_be_bytes());
        encoded.extend_from_slice(&self.limits.max_chunk_bytes().to_be_bytes());
        push_text_field(
            &mut encoded,
            "Hello",
            "implementation-name",
            &self.implementation.name,
        )?;
        push_text_field(
            &mut encoded,
            "Hello",
            "implementation-version",
            &self.implementation.version,
        )?;

        let capability_count =
            u16::try_from(self.capabilities.len()).map_err(|_| ProtocolError::FieldTooLarge {
                message: "Hello",
                field: "capabilities",
                len: self.capabilities.len(),
            })?;
        encoded.extend_from_slice(&capability_count.to_be_bytes());
        for capability in self.capabilities.iter() {
            encoded.extend_from_slice(&capability.id().to_be_bytes());
        }

        Ok(encoded)
    }

    fn decode_body(encoded: &[u8]) -> Result<Self, ProtocolError> {
        let mut cursor = Cursor::new("Hello", encoded);

        let role = PeerRole::from_wire(cursor.read_u8("role")?)?;
        let versions = VersionRange::new(
            ProtocolVersion::new(cursor.read_u16("min-version")?),
            ProtocolVersion::new(cursor.read_u16("max-version")?),
        )?;
        let limits = Limits::new(
            cursor.read_u32("max-frame-bytes")?,
            cursor.read_u32("max-chunk-bytes")?,
        )?;
        let implementation = ImplementationInfo::new(
            cursor.read_string("implementation-name")?,
            cursor.read_string("implementation-version")?,
        );

        let capability_count = cursor.read_u16("capability-count")?;
        let mut capabilities = Vec::with_capacity(capability_count as usize);
        for _ in 0..capability_count {
            capabilities.push(Capability::from_id(cursor.read_u16("capability-id")?)?);
        }

        Ok(Self {
            role,
            implementation,
            versions,
            capabilities: CapabilitySet::new(capabilities),
            limits,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NegotiatedSession {
    pub version: ProtocolVersion,
    pub capabilities: CapabilitySet,
    pub limits: Limits,
    pub local_role: PeerRole,
    pub remote_role: PeerRole,
    pub local_implementation: ImplementationInfo,
    pub remote_implementation: ImplementationInfo,
}

impl NegotiatedSession {
    pub fn require_capabilities(
        &self,
        required: impl IntoIterator<Item = Capability>,
    ) -> Result<(), ProtocolError> {
        for capability in required {
            if !self.capabilities.contains(capability) {
                return Err(ProtocolError::MissingRequiredCapability { capability });
            }
        }

        Ok(())
    }
}

pub fn current_implementation() -> ImplementationInfo {
    ImplementationInfo::new("oni", env!("CARGO_PKG_VERSION"))
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
#[repr(u16)]
pub enum MessageKind {
    Hello = 1,
}

impl MessageKind {
    pub const fn id(self) -> u16 {
        self as u16
    }

    fn from_id(id: u16) -> Result<Self, ProtocolError> {
        match id {
            1 => Ok(Self::Hello),
            _ => Err(ProtocolError::UnknownMessageKind { kind: id }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    Hello(Hello),
}

impl Message {
    pub fn encode(&self) -> Result<Vec<u8>, ProtocolError> {
        match self {
            Self::Hello(hello) => {
                let mut encoded = Vec::new();
                encoded.extend_from_slice(&MessageKind::Hello.id().to_be_bytes());
                encoded.extend_from_slice(&hello.encode_body()?);
                Ok(encoded)
            }
        }
    }

    pub fn decode(encoded: &[u8]) -> Result<Self, ProtocolError> {
        // Messages start with a numeric kind id so the transport layer never
        // needs to understand their semantics.
        let mut cursor = Cursor::new("Message", encoded);
        let kind = MessageKind::from_id(cursor.read_u16("kind")?)?;
        let body = cursor.remaining();

        match kind {
            MessageKind::Hello => Ok(Self::Hello(Hello::decode_body(body)?)),
        }
    }
}

fn push_text_field(
    encoded: &mut Vec<u8>,
    message: &'static str,
    field: &'static str,
    value: &str,
) -> Result<(), ProtocolError> {
    let bytes = value.as_bytes();
    let len = u16::try_from(bytes.len()).map_err(|_| ProtocolError::FieldTooLarge {
        message,
        field,
        len: bytes.len(),
    })?;

    encoded.extend_from_slice(&len.to_be_bytes());
    encoded.extend_from_slice(bytes);

    Ok(())
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

    fn read_u8(&mut self, field: &'static str) -> Result<u8, ProtocolError> {
        let bytes = self.read_bytes(field, 1)?;
        Ok(bytes[0])
    }

    fn read_u16(&mut self, field: &'static str) -> Result<u16, ProtocolError> {
        let bytes = self.read_bytes(field, 2)?;
        Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
    }

    fn read_u32(&mut self, field: &'static str) -> Result<u32, ProtocolError> {
        let bytes = self.read_bytes(field, 4)?;
        Ok(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn read_string(&mut self, field: &'static str) -> Result<String, ProtocolError> {
        let len = self.read_u16(field)? as usize;
        let bytes = self.read_bytes(field, len)?;

        String::from_utf8(bytes.to_vec()).map_err(|_| ProtocolError::InvalidTextField {
            message: self.message,
            field,
        })
    }

    fn read_bytes(&mut self, field: &'static str, len: usize) -> Result<&'a [u8], ProtocolError> {
        // A tiny cursor keeps binary decoding explicit and easy to audit.
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

    fn remaining(&self) -> &'a [u8] {
        &self.bytes[self.offset..]
    }
}

#[cfg(test)]
mod tests {
    use super::{
        current_implementation, Capability, CapabilitySet, ChunkerId, Hello, ImplementationInfo,
        Limits, Message, OperationKind, PeerRole, ProtocolVersion, VersionRange,
    };
    use crate::error::ProtocolError;

    #[test]
    fn rejects_inverted_version_ranges() {
        let error =
            VersionRange::new(ProtocolVersion::new(3), ProtocolVersion::new(2)).unwrap_err();

        assert_eq!(
            error,
            ProtocolError::InvalidVersionRange {
                min: ProtocolVersion::new(3),
                max: ProtocolVersion::new(2),
            }
        );
    }

    #[test]
    fn rejects_zero_limits() {
        let error = Limits::new(0, 64 * 1024).unwrap_err();

        assert_eq!(
            error,
            ProtocolError::InvalidLimit {
                field: "max-frame-bytes",
                value: 0,
            }
        );
    }

    #[test]
    fn negotiates_highest_shared_version_and_common_capabilities() {
        let local = Hello::new(
            PeerRole::Coordinator,
            ImplementationInfo::new("oni", "1.0.0"),
            VersionRange::new(ProtocolVersion::new(1), ProtocolVersion::new(3)).unwrap(),
            CapabilitySet::from([
                Capability::WholeFileTransfer,
                Capability::FastCdcChunker,
                Capability::StructuredStats,
            ]),
            Limits::new(8 * 1024 * 1024, 4 * 1024 * 1024).unwrap(),
        );
        let remote = Hello::new(
            PeerRole::Helper,
            ImplementationInfo::new("oni-helper", "1.0.0"),
            VersionRange::new(ProtocolVersion::new(2), ProtocolVersion::new(4)).unwrap(),
            CapabilitySet::from([
                Capability::WholeFileTransfer,
                Capability::CdcDeltaTransfer,
                Capability::FastCdcChunker,
            ]),
            Limits::new(4 * 1024 * 1024, 8 * 1024 * 1024).unwrap(),
        );

        let negotiated = Hello::negotiate(&local, &remote).unwrap();

        assert_eq!(negotiated.version, ProtocolVersion::new(3));
        assert_eq!(
            negotiated.capabilities,
            CapabilitySet::from([Capability::WholeFileTransfer, Capability::FastCdcChunker,])
        );
        assert_eq!(negotiated.limits.max_frame_bytes(), 4 * 1024 * 1024);
        assert_eq!(negotiated.limits.max_chunk_bytes(), 4 * 1024 * 1024);
    }

    #[test]
    fn rejects_peers_with_the_same_role() {
        let hello = Hello::new(
            PeerRole::Coordinator,
            ImplementationInfo::new("oni", "1.0.0"),
            VersionRange::exact(ProtocolVersion::new(1)),
            CapabilitySet::from([Capability::WholeFileTransfer]),
            Limits::new(1024, 1024).unwrap(),
        );

        let error = Hello::negotiate(&hello, &hello).unwrap_err();

        assert_eq!(
            error,
            ProtocolError::IncompatibleRoles {
                local: PeerRole::Coordinator,
                remote: PeerRole::Coordinator,
            }
        );
    }

    #[test]
    fn reports_when_no_protocol_version_overlaps() {
        let local = Hello::new(
            PeerRole::Coordinator,
            ImplementationInfo::new("oni", "1.0.0"),
            VersionRange::exact(ProtocolVersion::new(1)),
            CapabilitySet::from([Capability::WholeFileTransfer]),
            Limits::new(1024, 1024).unwrap(),
        );
        let remote = Hello::new(
            PeerRole::Helper,
            ImplementationInfo::new("oni-helper", "1.0.0"),
            VersionRange::exact(ProtocolVersion::new(2)),
            CapabilitySet::from([Capability::WholeFileTransfer]),
            Limits::new(1024, 1024).unwrap(),
        );

        let error = Hello::negotiate(&local, &remote).unwrap_err();

        assert_eq!(
            error,
            ProtocolError::NoSharedVersion {
                local: VersionRange::exact(ProtocolVersion::new(1)),
                remote: VersionRange::exact(ProtocolVersion::new(2)),
            }
        );
    }

    #[test]
    fn reports_missing_required_capabilities_by_name() {
        let local = Hello::new(
            PeerRole::Coordinator,
            ImplementationInfo::new("oni", "1.0.0"),
            VersionRange::exact(ProtocolVersion::new(1)),
            CapabilitySet::from([Capability::WholeFileTransfer]),
            Limits::new(1024, 1024).unwrap(),
        );
        let remote = Hello::new(
            PeerRole::Helper,
            ImplementationInfo::new("oni-helper", "1.0.0"),
            VersionRange::exact(ProtocolVersion::new(1)),
            CapabilitySet::from([Capability::WholeFileTransfer, Capability::StructuredStats]),
            Limits::new(1024, 1024).unwrap(),
        );

        let negotiated = Hello::negotiate(&local, &remote).unwrap();
        let error = negotiated
            .require_capabilities([Capability::StructuredStats])
            .unwrap_err();

        assert_eq!(
            error,
            ProtocolError::MissingRequiredCapability {
                capability: Capability::StructuredStats,
            }
        );
    }

    #[test]
    fn keeps_stable_numeric_ids_for_protocol_enums() {
        assert_eq!(Capability::WholeFileTransfer.id(), 0x0001);
        assert_eq!(Capability::FastCdcChunker.id(), 0x0100);
        assert_eq!(ChunkerId::SeqCdc.id(), 2);
        assert_eq!(OperationKind::DeleteDirectory.id(), 5);
    }

    #[test]
    fn round_trips_hello_messages() {
        let hello = Hello::for_current(
            PeerRole::Coordinator,
            current_implementation(),
            CapabilitySet::from([Capability::WholeFileTransfer, Capability::FastCdcChunker]),
            Limits::default(),
        );

        let encoded = Message::Hello(hello.clone()).encode().unwrap();
        let decoded = Message::decode(&encoded).unwrap();

        assert_eq!(decoded, Message::Hello(hello));
    }

    #[test]
    fn rejects_unknown_capability_ids_during_message_decode() {
        let mut encoded = Message::Hello(Hello::for_current(
            PeerRole::Coordinator,
            current_implementation(),
            CapabilitySet::from([Capability::WholeFileTransfer]),
            Limits::default(),
        ))
        .encode()
        .unwrap();

        let length = encoded.len();
        encoded[length - 2] = 0x99;
        encoded[length - 1] = 0x99;

        let error = Message::decode(&encoded).unwrap_err();

        assert_eq!(error, ProtocolError::UnknownCapabilityId { id: 0x9999 });
    }
}
