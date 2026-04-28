use crate::bridge::nats_publisher::{EventPublisher, PublishError, subject_for_event};
use crate::huly::connection::HulyEvent;
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

const RETRY_DELAYS: [Duration; 3] = [
    Duration::from_millis(100),
    Duration::from_millis(500),
    Duration::from_millis(2000),
];

/// Statistics tracked by the event loop
#[derive(Debug, Default, Clone)]
pub struct EventLoopStats {
    pub events_forwarded: u64,
    pub events_failed: u64,
}

/// Run the event loop: receive Huly events, forward to NATS
pub async fn run_event_loop(
    mut events: mpsc::Receiver<HulyEvent>,
    publisher: Arc<dyn EventPublisher>,
    subject_prefix: &str,
    cancel: CancellationToken,
) -> EventLoopStats {
    let mut stats = EventLoopStats::default();

    info!("event loop started");

    loop {
        tokio::select! {
            event = events.recv() => {
                match event {
                    Some(huly_event) => {
                        match forward_event(&huly_event, &publisher, subject_prefix).await {
                            Ok(()) => {
                                stats.events_forwarded += 1;
                            }
                            Err(e) if e.is_transient() => {
                                let mut last_err = e;
                                let mut succeeded = false;
                                for (attempt, delay) in RETRY_DELAYS.iter().enumerate() {
                                    warn!(attempt = attempt + 1, error = %last_err, "transient publish error, retrying");
                                    tokio::time::sleep(*delay).await;
                                    match forward_event(&huly_event, &publisher, subject_prefix).await {
                                        Ok(()) => {
                                            info!(attempt = attempt + 1, "event forwarded after retry");
                                            stats.events_forwarded += 1;
                                            succeeded = true;
                                            break;
                                        }
                                        Err(e) => {
                                            last_err = e;
                                        }
                                    }
                                }
                                if !succeeded {
                                    error!(error = %last_err, "failed to forward event after retries");
                                    stats.events_failed += 1;
                                }
                            }
                            Err(e) => {
                                error!(error = %e, "failed to forward event");
                                stats.events_failed += 1;
                            }
                        }
                    }
                    None => {
                        warn!("event channel closed, stopping event loop");
                        break;
                    }
                }
            }
            _ = cancel.cancelled() => {
                info!("event loop cancelled");
                break;
            }
        }
    }

    info!(
        forwarded = stats.events_forwarded,
        failed = stats.events_failed,
        "event loop stopped"
    );

    stats
}

async fn forward_event(
    event: &HulyEvent,
    publisher: &Arc<dyn EventPublisher>,
    subject_prefix: &str,
) -> Result<(), PublishError> {
    let event_type = extract_event_type(&event.result);
    let subject = subject_for_event(subject_prefix, event_type);

    let payload = serde_json::to_vec(&event.result)
        .map_err(|e| PublishError::Serialization(e.to_string()))?;

    publisher.publish(&subject, &payload).await
}

fn extract_event_type(result: &Option<Value>) -> &str {
    result
        .as_ref()
        .and_then(|v| v.get("event"))
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::nats_publisher::MockEventPublisher;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    #[tokio::test]
    async fn forwards_events_to_publisher() {
        let (tx, rx) = mpsc::channel(16);
        let cancel = CancellationToken::new();

        type Captured = Arc<Mutex<Vec<(String, Vec<u8>)>>>;
        let published: Captured = Arc::new(Mutex::new(vec![]));
        let published_clone = published.clone();

        let mut mock = MockEventPublisher::new();
        mock.expect_publish()
            .returning(move |subject, payload| {
                published_clone
                    .lock()
                    .unwrap()
                    .push((subject.to_string(), payload.to_vec()));
                Box::pin(async { Ok(()) })
            });

        // Send events
        tx.send(HulyEvent {
            result: Some(json!({"event": "tx", "data": "hello"})),
        })
        .await
        .unwrap();

        tx.send(HulyEvent {
            result: Some(json!({"event": "notification", "msg": "hi"})),
        })
        .await
        .unwrap();

        // Close channel to end loop
        drop(tx);

        let stats = run_event_loop(rx, Arc::new(mock), "huly", cancel).await;

        assert_eq!(stats.events_forwarded, 2);
        assert_eq!(stats.events_failed, 0);

        let published = published.lock().unwrap();
        assert_eq!(published.len(), 2);
        assert_eq!(published[0].0, "huly.events.tx");
        assert_eq!(published[1].0, "huly.events.notification");
    }

    #[tokio::test]
    async fn handles_publish_failure() {
        let (tx, rx) = mpsc::channel(16);
        let cancel = CancellationToken::new();

        let mut mock = MockEventPublisher::new();
        mock.expect_publish()
            .returning(|_, _| {
                Box::pin(async { Err(PublishError::Nats("timeout".into())) })
            });

        tx.send(HulyEvent {
            result: Some(json!({"event": "tx"})),
        })
        .await
        .unwrap();
        drop(tx);

        let stats = run_event_loop(rx, Arc::new(mock), "huly", cancel).await;
        assert_eq!(stats.events_forwarded, 0);
        assert_eq!(stats.events_failed, 1);
    }

    #[tokio::test]
    async fn stops_on_cancellation() {
        let (_tx, rx) = mpsc::channel(16);
        let cancel = CancellationToken::new();

        let mock = MockEventPublisher::new();
        let cancel_clone = cancel.clone();

        let handle = tokio::spawn(async move {
            run_event_loop(rx, Arc::new(mock), "huly", cancel_clone).await
        });

        // Cancel after short delay
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        cancel.cancel();

        let stats = handle.await.unwrap();
        assert_eq!(stats.events_forwarded, 0);
    }

    #[test]
    fn extract_event_type_from_result() {
        assert_eq!(
            extract_event_type(&Some(json!({"event": "tx"}))),
            "tx"
        );
        assert_eq!(
            extract_event_type(&Some(json!({"event": "doc.created"}))),
            "doc.created"
        );
        assert_eq!(extract_event_type(&Some(json!({}))), "unknown");
        assert_eq!(extract_event_type(&None), "unknown");
    }

    #[tokio::test(start_paused = true)]
    async fn retries_transient_errors_then_succeeds() {
        let (tx, rx) = mpsc::channel(16);
        let cancel = CancellationToken::new();

        let call_count = Arc::new(AtomicUsize::new(0));
        let call_count_clone = call_count.clone();

        let mut mock = MockEventPublisher::new();
        mock.expect_publish()
            .returning(move |_, _| {
                let n = call_count_clone.fetch_add(1, Ordering::SeqCst);
                Box::pin(async move {
                    if n < 2 {
                        Err(PublishError::Nats("timeout".into()))
                    } else {
                        Ok(())
                    }
                })
            });

        tx.send(HulyEvent {
            result: Some(json!({"event": "tx"})),
        })
        .await
        .unwrap();
        drop(tx);

        let stats = run_event_loop(rx, Arc::new(mock), "huly", cancel).await;
        assert_eq!(stats.events_forwarded, 1);
        assert_eq!(stats.events_failed, 0);
        assert_eq!(call_count.load(Ordering::SeqCst), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn gives_up_after_max_retries() {
        let (tx, rx) = mpsc::channel(16);
        let cancel = CancellationToken::new();

        let call_count = Arc::new(AtomicUsize::new(0));
        let call_count_clone = call_count.clone();

        let mut mock = MockEventPublisher::new();
        mock.expect_publish()
            .returning(move |_, _| {
                call_count_clone.fetch_add(1, Ordering::SeqCst);
                Box::pin(async { Err(PublishError::Nats("timeout".into())) })
            });

        tx.send(HulyEvent {
            result: Some(json!({"event": "tx"})),
        })
        .await
        .unwrap();
        drop(tx);

        let stats = run_event_loop(rx, Arc::new(mock), "huly", cancel).await;
        assert_eq!(stats.events_forwarded, 0);
        assert_eq!(stats.events_failed, 1);
        // Initial attempt + 3 retries = 4 total calls
        assert_eq!(call_count.load(Ordering::SeqCst), 4);
    }

    #[tokio::test]
    async fn does_not_retry_serialization_errors() {
        let (tx, rx) = mpsc::channel(16);
        let cancel = CancellationToken::new();

        let call_count = Arc::new(AtomicUsize::new(0));
        let call_count_clone = call_count.clone();

        let mut mock = MockEventPublisher::new();
        mock.expect_publish()
            .returning(move |_, _| {
                call_count_clone.fetch_add(1, Ordering::SeqCst);
                Box::pin(async { Err(PublishError::Serialization("bad format".into())) })
            });

        tx.send(HulyEvent {
            result: Some(json!({"event": "tx"})),
        })
        .await
        .unwrap();
        drop(tx);

        let stats = run_event_loop(rx, Arc::new(mock), "huly", cancel).await;
        assert_eq!(stats.events_forwarded, 0);
        assert_eq!(stats.events_failed, 1);
        // Should NOT retry — only 1 call
        assert_eq!(call_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn unknown_event_type_uses_default_subject() {
        let (tx, rx) = mpsc::channel(16);
        let cancel = CancellationToken::new();

        let published: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(vec![]));
        let published_clone = published.clone();

        let mut mock = MockEventPublisher::new();
        mock.expect_publish()
            .returning(move |subject, _| {
                published_clone.lock().unwrap().push(subject.to_string());
                Box::pin(async { Ok(()) })
            });

        tx.send(HulyEvent {
            result: Some(json!({"data": "no event field"})),
        })
        .await
        .unwrap();
        drop(tx);

        run_event_loop(rx, Arc::new(mock), "app", cancel).await;

        let published = published.lock().unwrap();
        assert_eq!(published[0], "app.events.unknown");
    }
}
