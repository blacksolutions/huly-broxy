use crate::admin::health::HealthState;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

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

/// No-op notifier for platforms without systemd.
#[cfg(not(target_os = "linux"))]
pub struct SdNotifier;

#[cfg(not(target_os = "linux"))]
impl SystemNotifier for SdNotifier {
    fn notify_watchdog(&self) {}
}

/// Run the systemd watchdog pinger.
/// Pings systemd watchdog every `interval` as long as health checks pass.
pub async fn run_watchdog(
    health: HealthState,
    interval: Duration,
    cancel: CancellationToken,
    notifier: &dyn SystemNotifier,
) {
    tracing::info!(interval_secs = interval.as_secs(), "watchdog started");

    loop {
        tokio::select! {
            _ = tokio::time::sleep(interval) => {
                if health.is_ready() {
                    debug!("watchdog ping: healthy");
                    notifier.notify_watchdog();
                } else {
                    warn!(
                        huly = health.is_huly_connected(),
                        nats = health.is_nats_connected(),
                        "watchdog: not healthy, skipping ping"
                    );
                }
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
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

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
    async fn pings_when_healthy() {
        let health = HealthState::new();
        health.set_huly_connected(true);
        health.set_nats_connected(true);
        let cancel = CancellationToken::new();
        let (notifier, count) = CountingNotifier::new();

        let cancel_clone = cancel.clone();
        let handle = tokio::spawn(async move {
            run_watchdog(health, Duration::from_secs(5), cancel_clone, &notifier).await;
        });

        // Advance past one interval
        tokio::time::sleep(Duration::from_secs(6)).await;
        assert_eq!(count.load(Ordering::Relaxed), 1);

        cancel.cancel();
        handle.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn skips_when_huly_disconnected() {
        let health = HealthState::new();
        health.set_nats_connected(true);
        // huly NOT connected
        let cancel = CancellationToken::new();
        let (notifier, count) = CountingNotifier::new();

        let cancel_clone = cancel.clone();
        let handle = tokio::spawn(async move {
            run_watchdog(health, Duration::from_secs(5), cancel_clone, &notifier).await;
        });

        tokio::time::sleep(Duration::from_secs(6)).await;
        assert_eq!(count.load(Ordering::Relaxed), 0);

        cancel.cancel();
        handle.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn skips_when_nats_disconnected() {
        let health = HealthState::new();
        health.set_huly_connected(true);
        // nats NOT connected
        let cancel = CancellationToken::new();
        let (notifier, count) = CountingNotifier::new();

        let cancel_clone = cancel.clone();
        let handle = tokio::spawn(async move {
            run_watchdog(health, Duration::from_secs(5), cancel_clone, &notifier).await;
        });

        tokio::time::sleep(Duration::from_secs(6)).await;
        assert_eq!(count.load(Ordering::Relaxed), 0);

        cancel.cancel();
        handle.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn resumes_after_recovery() {
        let health = HealthState::new();
        // Start unhealthy
        let cancel = CancellationToken::new();
        let (notifier, count) = CountingNotifier::new();

        let health_clone = health.clone();
        let cancel_clone = cancel.clone();
        let handle = tokio::spawn(async move {
            run_watchdog(health_clone, Duration::from_secs(5), cancel_clone, &notifier).await;
        });

        // First interval: unhealthy, no ping
        tokio::time::sleep(Duration::from_secs(6)).await;
        assert_eq!(count.load(Ordering::Relaxed), 0);

        // Recover
        health.set_huly_connected(true);
        health.set_nats_connected(true);

        // Second interval: healthy, should ping
        tokio::time::sleep(Duration::from_secs(5)).await;
        assert_eq!(count.load(Ordering::Relaxed), 1);

        cancel.cancel();
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn watchdog_stops_on_cancel() {
        let health = HealthState::new();
        let cancel = CancellationToken::new();
        let (notifier, _count) = CountingNotifier::new();

        let cancel_clone = cancel.clone();
        let handle = tokio::spawn(async move {
            run_watchdog(health, Duration::from_secs(1), cancel_clone, &notifier).await;
        });

        tokio::time::sleep(Duration::from_millis(50)).await;
        cancel.cancel();

        tokio::time::timeout(Duration::from_secs(2), handle)
            .await
            .unwrap()
            .unwrap();
    }
}
