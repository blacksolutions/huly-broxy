use serde::Serialize;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Shared health state across the application
#[derive(Debug, Clone)]
pub struct HealthState {
    huly_connected: Arc<AtomicBool>,
    nats_connected: Arc<AtomicBool>,
}

impl HealthState {
    pub fn new() -> Self {
        Self {
            huly_connected: Arc::new(AtomicBool::new(false)),
            nats_connected: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn set_huly_connected(&self, connected: bool) {
        self.huly_connected.store(connected, Ordering::SeqCst);
    }

    pub fn set_nats_connected(&self, connected: bool) {
        self.nats_connected.store(connected, Ordering::SeqCst);
    }

    pub fn is_huly_connected(&self) -> bool {
        self.huly_connected.load(Ordering::SeqCst)
    }

    pub fn is_nats_connected(&self) -> bool {
        self.nats_connected.load(Ordering::SeqCst)
    }

    pub fn is_ready(&self) -> bool {
        self.is_huly_connected() && self.is_nats_connected()
    }

    pub fn status(&self) -> HealthStatus {
        HealthStatus {
            huly_connected: self.is_huly_connected(),
            nats_connected: self.is_nats_connected(),
            ready: self.is_ready(),
        }
    }
}

impl Default for HealthState {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Serialize)]
pub struct HealthStatus {
    pub huly_connected: bool,
    pub nats_connected: bool,
    pub ready: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initial_state_not_ready() {
        let health = HealthState::new();
        assert!(!health.is_ready());
        assert!(!health.is_huly_connected());
        assert!(!health.is_nats_connected());
    }

    #[test]
    fn ready_when_both_connected() {
        let health = HealthState::new();
        health.set_huly_connected(true);
        assert!(!health.is_ready());

        health.set_nats_connected(true);
        assert!(health.is_ready());
    }

    #[test]
    fn not_ready_if_only_nats() {
        let health = HealthState::new();
        health.set_nats_connected(true);
        assert!(!health.is_ready());
    }

    #[test]
    fn status_reflects_state() {
        let health = HealthState::new();
        health.set_huly_connected(true);
        health.set_nats_connected(true);

        let status = health.status();
        assert!(status.huly_connected);
        assert!(status.nats_connected);
        assert!(status.ready);
    }

    #[test]
    fn state_can_toggle() {
        let health = HealthState::new();
        health.set_huly_connected(true);
        assert!(health.is_huly_connected());

        health.set_huly_connected(false);
        assert!(!health.is_huly_connected());
    }

    #[test]
    fn clone_shares_state() {
        let health = HealthState::new();
        let health2 = health.clone();

        health.set_huly_connected(true);
        assert!(health2.is_huly_connected());
    }
}
