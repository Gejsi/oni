//! SSH helper launch wrapper.
//!
//! This module only builds and spawns the helper process. Session logic and
//! protocol semantics live above it.

use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, Stdio};

use crate::error::TransportError;
use crate::path::RemoteEndpoint;

/// A plain command description keeps SSH launch logic testable without
/// inspecting `std::process::Command` internals.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invocation {
    pub program: String,
    pub args: Vec<String>,
}

impl Invocation {
    pub fn helper(endpoint: &RemoteEndpoint) -> Self {
        // SSH is only the launch boundary. The helper itself always speaks the
        // internal stdio protocol once the process is running.
        Self {
            program: "ssh".to_string(),
            args: vec![
                ssh_destination(endpoint),
                "--".to_string(),
                "oni".to_string(),
                "internal".to_string(),
                "serve".to_string(),
                "--stdio".to_string(),
            ],
        }
    }

    fn into_command(self) -> Command {
        let mut command = Command::new(self.program);
        command.args(self.args);
        command.stdin(Stdio::piped());
        command.stdout(Stdio::piped());
        command.stderr(Stdio::piped());
        command
    }
}

#[derive(Debug)]
pub struct LaunchedHelper {
    pub child: Child,
    pub stdin: ChildStdin,
    pub stdout: ChildStdout,
    pub stderr: ChildStderr,
}

pub fn launch_helper(endpoint: &RemoteEndpoint) -> Result<LaunchedHelper, TransportError> {
    launch_invocation(Invocation::helper(endpoint))
}

fn launch_invocation(invocation: Invocation) -> Result<LaunchedHelper, TransportError> {
    // Build one human-readable target string so launch and missing-pipe errors
    // point at the exact helper command the session tried to start.
    let target = if invocation.args.is_empty() {
        invocation.program.clone()
    } else {
        format!("{} {}", invocation.program, invocation.args.join(" "))
    };
    let mut child = invocation
        .into_command()
        .spawn()
        .map_err(|source| TransportError::Launch {
            target: target.clone(),
            source,
        })?;

    let stdin = child.stdin.take().ok_or(TransportError::MissingPipe {
        pipe: "stdin",
        target: target.clone(),
    })?;
    let stdout = child.stdout.take().ok_or(TransportError::MissingPipe {
        pipe: "stdout",
        target: target.clone(),
    })?;
    let stderr = child.stderr.take().ok_or(TransportError::MissingPipe {
        pipe: "stderr",
        target,
    })?;

    Ok(LaunchedHelper {
        child,
        stdin,
        stdout,
        stderr,
    })
}

fn ssh_destination(endpoint: &RemoteEndpoint) -> String {
    match &endpoint.user {
        Some(user) => format!("{user}@{}", endpoint.host),
        None => endpoint.host.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::{launch_invocation, Invocation};
    use crate::error::TransportError;
    use crate::path::{parse_endpoint, Endpoint};

    #[test]
    fn builds_helper_invocation_with_an_explicit_user() {
        let endpoint = match parse_endpoint("alice@example.com:/srv/archive").unwrap() {
            Endpoint::Remote(endpoint) => endpoint,
            Endpoint::Local(_) => panic!("expected remote endpoint"),
        };

        let invocation = Invocation::helper(&endpoint);

        assert_eq!(invocation.program, "ssh");
        assert_eq!(
            invocation.args,
            vec![
                "alice@example.com",
                "--",
                "oni",
                "internal",
                "serve",
                "--stdio",
            ]
        );
    }

    #[test]
    fn builds_helper_invocation_without_a_user() {
        let endpoint = match parse_endpoint("buildbox:/srv/archive").unwrap() {
            Endpoint::Remote(endpoint) => endpoint,
            Endpoint::Local(_) => panic!("expected remote endpoint"),
        };

        let invocation = Invocation::helper(&endpoint);

        assert_eq!(
            invocation.args,
            vec!["buildbox", "--", "oni", "internal", "serve", "--stdio",]
        );
    }

    #[test]
    fn surfaces_spawn_failures_cleanly() {
        let error = launch_invocation(Invocation {
            program: "/definitely/not-a-real-oni-ssh-binary".to_string(),
            args: Vec::new(),
        })
        .unwrap_err();

        match error {
            TransportError::Launch { target, .. } => {
                assert_eq!(target, "/definitely/not-a-real-oni-ssh-binary");
            }
            other => panic!("expected launch error, got {other:?}"),
        }
    }
}
