//! A [`Connector`](super::Connector) with no file server behind it.
//!
//! Not test-only: it is compiled into the crate the same way
//! [`crate::device::MockBackend`] is, so the behaviour suites in `tests/` can drive
//! the whole flow — connect, open the database, write a record, close, disconnect —
//! without a share, a network or a credential that exists anywhere.
//!
//! It also records the calls it received, which is how the tests assert the two
//! rules that matter and are otherwise invisible: a share that was already mounted
//! is **never** connected, and a connection this session made is disconnected
//! **once**.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use super::{Connected, Connector, Credential, Result, ShareTarget, SmbError};

/// What the mock does when asked to connect.
#[derive(Debug, Clone)]
pub enum MockOutcome {
    /// The share is already mounted at this path; `connect` must not be called.
    Adopt(PathBuf),
    /// Connect successfully, reporting this path as the share's root.
    Connect(PathBuf),
    /// Refuse the identity, with this reason.
    Refuse(String),
    /// The share, or the network, is not there.
    Unreachable(String),
}

/// Records what it was asked to do, and answers with a fixed outcome.
#[derive(Debug, Clone)]
pub struct MockConnector {
    outcome: MockOutcome,
    calls: Arc<Mutex<Vec<String>>>,
    /// The credential the last `connect` was given, without its password —
    /// enough to assert that an explicit account was not silently replaced by
    /// the signed-in user.
    identities: Arc<Mutex<Vec<String>>>,
}

impl MockConnector {
    pub fn new(outcome: MockOutcome) -> Self {
        Self {
            outcome,
            calls: Arc::new(Mutex::new(Vec::new())),
            identities: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// A share the operator had already mounted.
    pub fn adopting(root: impl Into<PathBuf>) -> Self {
        Self::new(MockOutcome::Adopt(root.into()))
    }

    /// A share this session connects.
    pub fn connecting(root: impl Into<PathBuf>) -> Self {
        Self::new(MockOutcome::Connect(root.into()))
    }

    /// A server that refuses the identity presented.
    pub fn refusing(reason: impl Into<String>) -> Self {
        Self::new(MockOutcome::Refuse(reason.into()))
    }

    /// A server, or a network, that is not there.
    pub fn unreachable(reason: impl Into<String>) -> Self {
        Self::new(MockOutcome::Unreachable(reason.into()))
    }

    /// The calls this connector received, in order.
    pub fn calls(&self) -> Arc<Mutex<Vec<String>>> {
        Arc::clone(&self.calls)
    }

    /// The identities `connect` was asked to present, in order. Never a password.
    pub fn identities(&self) -> Arc<Mutex<Vec<String>>> {
        Arc::clone(&self.identities)
    }

    fn record(&self, call: &str) {
        if let Ok(mut calls) = self.calls.lock() {
            calls.push(call.to_owned());
        }
    }
}

impl Connector for MockConnector {
    fn label(&self) -> &'static str {
        "mock"
    }

    fn existing_root(&self, _target: &ShareTarget) -> Option<PathBuf> {
        self.record("existing_root");
        match &self.outcome {
            MockOutcome::Adopt(root) => Some(root.clone()),
            _ => None,
        }
    }

    fn connect(&self, target: &ShareTarget, credential: &Credential) -> Result<Connected> {
        self.record("connect");
        if let Ok(mut identities) = self.identities.lock() {
            identities.push(credential.describe());
        }
        match &self.outcome {
            MockOutcome::Connect(root) => {
                // A real connector makes the share's root reachable; a test that
                // then creates a database in it needs the directory to be there.
                let _ = std::fs::create_dir_all(root);
                Ok(Connected::ours(root.clone()))
            }
            MockOutcome::Refuse(reason) => Err(SmbError::Refused {
                share: target.describe(),
                identity: credential.describe(),
                reason: reason.clone(),
            }),
            MockOutcome::Unreachable(reason) => Err(SmbError::Unreachable {
                share: target.describe(),
                reason: reason.clone(),
            }),
            // `existing_root` already answered, so this cannot happen — and if it
            // ever does, the test that caused it should fail loudly rather than
            // quietly succeed.
            MockOutcome::Adopt(_) => Err(SmbError::Unreachable {
                share: target.describe(),
                reason: "the mock was configured to adopt an existing mount, but was asked to \
                         connect — the probe was skipped"
                    .into(),
            }),
        }
    }

    fn disconnect(&self, _attachment: &super::Attachment) -> Result<()> {
        self.record("disconnect");
        Ok(())
    }
}

/// A connector that refuses to disconnect, for the one path that has to survive it.
#[derive(Debug)]
pub struct StubbornConnector {
    pub root: PathBuf,
}

impl Connector for StubbornConnector {
    fn label(&self) -> &'static str {
        "stubborn (test)"
    }

    fn existing_root(&self, _target: &ShareTarget) -> Option<PathBuf> {
        None
    }

    fn connect(&self, _target: &ShareTarget, _credential: &Credential) -> Result<Connected> {
        let _ = std::fs::create_dir_all(&self.root);
        Ok(Connected::ours(self.root.clone()))
    }

    fn disconnect(&self, attachment: &super::Attachment) -> Result<()> {
        Err(SmbError::DetachFailed {
            share: attachment.target.describe(),
            reason: "the share is busy".into(),
        })
    }
}
