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
    FixedDeltaTransfer,
    CdcDeltaTransfer,
    FixedChunker = 0x0100,
    FastCdcChunker,
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
}

impl fmt::Display for Capability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WholeFileTransfer => f.write_str("whole-file-transfer"),
            Self::FixedDeltaTransfer => f.write_str("fixed-delta-transfer"),
            Self::CdcDeltaTransfer => f.write_str("cdc-delta-transfer"),
            Self::FixedChunker => f.write_str("fixed-chunker"),
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
    Fixed = 1,
    FastCdc,
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
            Self::Fixed => f.write_str("fixed"),
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

#[cfg(test)]
mod tests {
    use super::{
        Capability, CapabilitySet, ChunkerId, Hello, ImplementationInfo, Limits, OperationKind,
        PeerRole, ProtocolVersion, VersionRange,
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
                Capability::FixedDeltaTransfer,
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
        assert_eq!(Capability::FastCdcChunker.id(), 0x0101);
        assert_eq!(ChunkerId::SeqCdc.id(), 3);
        assert_eq!(OperationKind::DeleteDirectory.id(), 5);
    }
}
