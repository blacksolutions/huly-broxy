//! Integration test for the on-demand bridge lookup responder.
//!
//! Verifies that a freshly-spawned MCP-style requester can pull current
//! bridge state via NATS request/reply on `LOOKUP_SUBJECT` without waiting
//! for a periodic announcement.

mod common;

use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use huly_bridge::admin::health::HealthState;
use huly_bridge::bridge::announcer::{self, SocialIdHandle};
use huly_bridge::bridge::schema_resolver::SchemaHandle;
use huly_common::announcement::{BridgeAnnouncement, LOOKUP_SUBJECT};
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn lookup_responder_replies_with_current_announcement() {
    let Some((_nats, url)) = common::ephemeral_nats().await else {
        return;
    };

    let client = async_nats::connect(&url).await.expect("connect to NATS");

    let health = HealthState::new();
    health.set_huly_connected(true);
    health.set_nats_connected(true);

    let social_id_handle: SocialIdHandle = Arc::new(RwLock::new(Some("soc-42".into())));
    let cancel = CancellationToken::new();
    let start_time = Instant::now();

    let responder_client = client.clone();
    let responder_cancel = cancel.clone();
    let schema_handle = SchemaHandle::new();
    let responder = tokio::spawn(async move {
        announcer::run_lookup_responder(
            responder_client,
            "ws-test".into(),
            "http://bridge.test:9095".into(),
            health,
            start_time,
            social_id_handle,
            schema_handle,
            responder_cancel,
        )
        .await;
    });

    // Give the responder a moment to subscribe before issuing the request.
    tokio::time::sleep(Duration::from_millis(100)).await;

    let reply = client
        .request(LOOKUP_SUBJECT.to_string(), Vec::new().into())
        .await
        .expect("lookup request");

    let announcement: BridgeAnnouncement =
        serde_json::from_slice(&reply.payload).expect("parse reply");

    assert_eq!(announcement.workspace, "ws-test");
    assert_eq!(announcement.proxy_url, "http://bridge.test:9095");
    assert!(announcement.huly_connected);
    assert!(announcement.nats_connected);
    assert!(announcement.ready);
    assert_eq!(announcement.social_id.as_deref(), Some("soc-42"));

    cancel.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(2), responder).await;
}

#[tokio::test]
async fn lookup_responder_ignores_requests_without_reply_to() {
    let Some((_nats, url)) = common::ephemeral_nats().await else {
        return;
    };

    let client = async_nats::connect(&url).await.expect("connect to NATS");
    let health = HealthState::new();
    let social_id_handle: SocialIdHandle = Arc::new(RwLock::new(None));
    let cancel = CancellationToken::new();

    let responder_client = client.clone();
    let responder_cancel = cancel.clone();
    let schema_handle = SchemaHandle::new();
    let responder = tokio::spawn(async move {
        announcer::run_lookup_responder(
            responder_client,
            "ws-test".into(),
            "http://bridge.test:9095".into(),
            health,
            Instant::now(),
            social_id_handle,
            schema_handle,
            responder_cancel,
        )
        .await;
    });

    tokio::time::sleep(Duration::from_millis(100)).await;

    // Plain publish (no reply-to). The responder must drop it cleanly
    // without panicking.
    client
        .publish(LOOKUP_SUBJECT.to_string(), Vec::new().into())
        .await
        .expect("publish");
    client.flush().await.expect("flush");

    // Give the responder time to process and drop the message.
    tokio::time::sleep(Duration::from_millis(100)).await;

    cancel.cancel();
    let join = tokio::time::timeout(Duration::from_secs(2), responder).await;
    assert!(join.is_ok(), "responder must shut down cleanly after no-reply request");
}
