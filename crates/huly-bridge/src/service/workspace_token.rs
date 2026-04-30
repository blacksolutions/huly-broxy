//! Workspace-scoped token cache.
//!
//! Mirrors [`huly_client::rest::ServerConfigCache`] — an `Arc<RwLock<Option<SecretString>>>`
//! with `set` / `get` accessors. Cheap to clone (reference-counted).
//!
//! Populated inside the reconnect loop every time `selectWorkspace` resolves
//! and read by collaborator HTTP handlers.

use secrecy::SecretString;
use std::sync::{Arc, RwLock};

/// Process-wide cache for the workspace-scoped token issued by `selectWorkspace`.
///
/// `None` means the bridge has not yet completed its first workspace login
/// (still connecting / reconnecting). Handlers that require this token should
/// return `503 Service Unavailable` when the cache is empty.
#[derive(Debug, Clone, Default)]
pub struct WorkspaceTokenCache {
    inner: Arc<RwLock<Option<SecretString>>>,
}

impl WorkspaceTokenCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Store the workspace-scoped token.
    pub fn set(&self, token: SecretString) {
        let mut guard = self.inner.write().expect("WorkspaceTokenCache write poisoned");
        *guard = Some(token);
    }

    /// Read the current token (cloned).
    pub fn get(&self) -> Option<SecretString> {
        self.inner
            .read()
            .expect("WorkspaceTokenCache read poisoned")
            .clone()
    }

    /// `true` iff a token is currently cached.
    pub fn is_available(&self) -> bool {
        self.inner
            .read()
            .expect("WorkspaceTokenCache read poisoned")
            .is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use secrecy::ExposeSecret;

    #[test]
    fn starts_empty() {
        let cache = WorkspaceTokenCache::new();
        assert!(!cache.is_available());
        assert!(cache.get().is_none());
    }

    #[test]
    fn set_and_get_roundtrip() {
        let cache = WorkspaceTokenCache::new();
        cache.set(SecretString::from("my-ws-token"));
        assert!(cache.is_available());
        let tok = cache.get().unwrap();
        assert_eq!(tok.expose_secret(), "my-ws-token");
    }

    #[test]
    fn clone_shares_state() {
        let a = WorkspaceTokenCache::new();
        let b = a.clone();
        a.set(SecretString::from("shared-token"));
        let tok = b.get().unwrap();
        assert_eq!(tok.expose_secret(), "shared-token");
    }

    #[test]
    fn overwrite_replaces_value() {
        let cache = WorkspaceTokenCache::new();
        cache.set(SecretString::from("first"));
        cache.set(SecretString::from("second"));
        let tok = cache.get().unwrap();
        assert_eq!(tok.expose_secret(), "second");
    }
}
