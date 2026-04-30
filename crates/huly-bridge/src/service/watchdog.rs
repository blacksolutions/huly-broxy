//! Systemd watchdog pinger.
//!
//! Post-P4 the bridge no longer exposes a `/health` HTTP endpoint, so the
//! watchdog has nothing to read inline. Instead we ping unconditionally
//! while the lifecycle loop is alive — when the WS or NATS connection drops
//! the loop exits and the watchdog is cancelled, which is exactly what
//! systemd needs to restart the unit.

use std::time::Duration;
use tokio_util::sync::CancellationToken;
use tracing::debug;

/// Abstraction over systemd notification for testability.
pub trait SystemNotifier: Send + Sync {
    fn notify_watchdog(&self);
}

/// Production notifier that calls sd_notify (Linux/systemd only).
#[cfg(target_os = "linux")]
pub struct SdNotifier;

#[cfg(target_os = "linux")]
impl SystemNotifier for SdNotifier {
    fn notify_watchdog(&self) {
        let _ = sd_notify::notify(false, &[sd_notify::NotifyState::Watchdog]);
    }
}

#[cfg(not(target_os = "linux"))]
pub struct SdNotifier;

#[cfg(not(target_os = "linux"))]
impl SystemNotifier for SdNotifier {
    fn notify_watchdog(&self) {}
}

/// Run the systemd watchdog pinger.
///
/// Pings every `interval` until cancelled. The lifecycle loop owns the
/// cancellation token and drops it on shutdown / unrecoverable disconnect.
pub async fn run_watchdog_simple(
    interval: Duration,
    cancel: CancellationToken,
    notifier: &dyn SystemNotifier,
) {
    tracing::info!(interval_secs = interval.as_secs(), "watchdog started");
    loop {
        tokio::select! {
            _ = tokio::time::sleep(interval) => {
                debug!("watchdog ping");
                notifier.notify_watchdog();
            }
            _ = cancel.cancelled() => {
                tracing::info!("watchdog stopped");
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    struct CountingNotifier {
        count: Arc<AtomicU32>,
    }

    impl CountingNotifier {
        fn new() -> (Self, Arc<AtomicU32>) {
            let count = Arc::new(AtomicU32::new(0));
            (Self { count: count.clone() }, count)
        }
    }

    impl SystemNotifier for CountingNotifier {
        fn notify_watchdog(&self) {
            self.count.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn pings_every_interval() {
        let cancel = CancellationToken::new();
        let (notifier, count) = CountingNotifier::new();

        let cancel_clone = cancel.clone();
        let handle = tokio::spawn(async move {
            run_watchdog_simple(Duration::from_secs(5), cancel_clone, &notifier).await;
        });

        tokio::time::sleep(Duration::from_secs(6)).await;
        assert_eq!(count.load(Ordering::Relaxed), 1);

        tokio::time::sleep(Duration::from_secs(5)).await;
        assert_eq!(count.load(Ordering::Relaxed), 2);

        cancel.cancel();
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn watchdog_stops_on_cancel() {
        let cancel = CancellationToken::new();
        let (notifier, _count) = CountingNotifier::new();

        let cancel_clone = cancel.clone();
        let handle = tokio::spawn(async move {
            run_watchdog_simple(Duration::from_millis(20), cancel_clone, &notifier).await;
        });

        tokio::time::sleep(Duration::from_millis(50)).await;
        cancel.cancel();
        tokio::time::timeout(Duration::from_secs(2), handle)
            .await
            .unwrap()
            .unwrap();
    }
}
