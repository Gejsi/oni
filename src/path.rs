//! Endpoint parsing for the public CLI.
//!
//! This module keeps the user-facing path rules in one place so session code
//! does not need to guess whether a value is local or remote.

use std::path::PathBuf;
#[cfg(test)]
use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::PathError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Endpoint {
    Local(LocalEndpoint),
    Remote(RemoteEndpoint),
}

impl std::fmt::Display for Endpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Local(endpoint) => endpoint.fmt(f),
            Self::Remote(endpoint) => endpoint.fmt(f),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalEndpoint {
    raw: String,
    pub path: PathBuf,
    // Keep this now so later rsync-like root semantics do not need to reparse
    // the original CLI string.
    pub had_trailing_separator: bool,
}

impl LocalEndpoint {
    pub fn new(spec: &str) -> Self {
        Self {
            raw: spec.to_string(),
            path: PathBuf::from(spec),
            had_trailing_separator: has_trailing_separator(spec),
        }
    }
}

impl std::fmt::Display for LocalEndpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.raw)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteEndpoint {
    raw: String,
    pub user: Option<String>,
    pub host: String,
    pub path: PathBuf,
    pub had_trailing_separator: bool,
}

impl std::fmt::Display for RemoteEndpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.raw)
    }
}

/// Parse a CLI endpoint into a typed local or remote form.
pub fn parse_endpoint(spec: &str) -> Result<Endpoint, PathError> {
    if let Some(remote) = try_parse_remote(spec)? {
        return Ok(Endpoint::Remote(remote));
    }

    Ok(Endpoint::Local(LocalEndpoint::new(spec)))
}

fn try_parse_remote(spec: &str) -> Result<Option<RemoteEndpoint>, PathError> {
    if looks_like_windows_drive(spec) {
        return Ok(None);
    }

    let Some((authority, path)) = spec.split_once(':') else {
        return Ok(None);
    };

    // Only the first path segment gets to decide "remote or local".
    // That keeps `/tmp/a:b` and `dir/name:1` as local paths.
    //
    // Bare `name:path` follows the familiar rsync/scp rule and is treated as
    // remote. A local path with `:` in its first segment must therefore be
    // disambiguated with `./name:path` or another separator-bearing prefix.
    if authority.is_empty() || authority.contains('/') || authority.contains('\\') {
        return Ok(None);
    }

    if path.is_empty() {
        return Err(PathError::MissingRemotePath {
            spec: spec.to_string(),
        });
    }

    let (user, host) = match authority.split_once('@') {
        Some((user, host)) => {
            if user.is_empty() {
                return Err(PathError::MissingRemoteUser {
                    spec: spec.to_string(),
                });
            }

            (Some(user.to_string()), host)
        }
        None => (None, authority),
    };

    if host.is_empty() {
        return Err(PathError::MissingRemoteHost {
            spec: spec.to_string(),
        });
    }

    Ok(Some(RemoteEndpoint {
        raw: spec.to_string(),
        user,
        host: host.to_string(),
        path: PathBuf::from(path),
        had_trailing_separator: has_trailing_separator(path),
    }))
}

fn has_trailing_separator(spec: &str) -> bool {
    spec.ends_with('/') || spec.ends_with('\\')
}

fn looks_like_windows_drive(spec: &str) -> bool {
    let bytes = spec.as_bytes();
    bytes.len() >= 3 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic()
}

#[cfg(test)]
// Tests create real trees on disk.
// The label keeps leftover paths readable when a case fails.
pub(crate) fn temp_path(label: &str) -> PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);

    std::env::temp_dir().join(format!("oni-{label}-{unique}"))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{parse_endpoint, Endpoint};
    use crate::error::PathError;

    #[test]
    fn parses_local_paths_with_colons_after_a_separator() {
        let path = parse_endpoint("/tmp/notes:2026.txt").unwrap();

        let Endpoint::Local(local) = path else {
            panic!("expected local path");
        };

        assert_eq!(local.path, PathBuf::from("/tmp/notes:2026.txt"));
    }

    #[test]
    fn keeps_disambiguated_relative_names_with_colons_local() {
        let path = parse_endpoint("./notes:2026.txt").unwrap();

        let Endpoint::Local(local) = path else {
            panic!("expected local path");
        };

        assert_eq!(local.path, PathBuf::from("./notes:2026.txt"));
    }

    #[test]
    fn parses_remote_paths_with_optional_user() {
        let path = parse_endpoint("alice@example.com:/srv/archive").unwrap();

        let Endpoint::Remote(remote) = path else {
            panic!("expected remote path");
        };

        assert_eq!(remote.user.as_deref(), Some("alice"));
        assert_eq!(remote.host, "example.com");
        assert_eq!(remote.path, PathBuf::from("/srv/archive"));
    }

    #[test]
    fn parses_remote_paths_without_users() {
        let path = parse_endpoint("buildbox:archive").unwrap();

        let Endpoint::Remote(remote) = path else {
            panic!("expected remote path");
        };

        assert!(remote.user.is_none());
        assert_eq!(remote.host, "buildbox");
        assert_eq!(remote.path, PathBuf::from("archive"));
    }

    #[test]
    fn keeps_windows_drive_letters_local() {
        let path = parse_endpoint(r"C:\sync\target").unwrap();

        let Endpoint::Local(local) = path else {
            panic!("expected local path");
        };

        assert_eq!(local.path, PathBuf::from(r"C:\sync\target"));
    }

    #[test]
    fn rejects_remote_specs_without_a_path() {
        let error = parse_endpoint("buildbox:").unwrap_err();

        assert_eq!(
            error,
            PathError::MissingRemotePath {
                spec: "buildbox:".to_string(),
            }
        );
    }

    #[test]
    fn rejects_remote_specs_with_an_empty_user() {
        let error = parse_endpoint("@host:/srv/archive").unwrap_err();

        assert_eq!(
            error,
            PathError::MissingRemoteUser {
                spec: "@host:/srv/archive".to_string(),
            }
        );
    }
}
