use async_trait::async_trait;
use tracing::{debug, error};

#[async_trait]
#[cfg_attr(test, mockall::automock)]
pub trait EventPublisher: Send + Sync {
    async fn publish(&self, subject: &str, payload: &[u8]) -> Result<(), PublishError>;
    async fn flush(&self) -> Result<(), PublishError>;
}

#[derive(Debug, thiserror::Error)]
pub enum PublishError {
    #[error("nats publish failed: {0}")]
    Nats(String),

    #[error("serialization failed: {0}")]
    Serialization(String),
}

impl PublishError {
    pub fn is_transient(&self) -> bool {
        matches!(self, PublishError::Nats(_))
    }
}

/// Build a NATS subject for a Huly event type.
///
/// Subject taxonomy is **singular** — `<prefix>.event.<type>` — to
/// match `huly_common::announcement::EVENT_SUBJECT_PREFIX` and the
/// MCP-side schema invalidator subscription pattern. The plural
/// (`.events.`) form was an early-P4 typo; reconciled in P7.
pub fn subject_for_event(prefix: &str, event_type: &str) -> String {
    format!("{}.event.{}", prefix, event_type)
}

#[derive(Debug)]
pub struct NatsPublisher {
    client: async_nats::Client,
    subject_prefix: String,
}

impl NatsPublisher {
    pub async fn connect(
        url: &str,
        subject_prefix: Option<&str>,
        credentials: Option<&str>,
    ) -> Result<Self, PublishError> {
        let client = if let Some(creds_path) = credentials {
            let options =
                async_nats::ConnectOptions::with_credentials_file(creds_path)
                    .await
                    .map_err(|e| PublishError::Nats(format!("failed to load credentials: {e}")))?;
            options
                .connect(url)
                .await
                .map_err(|e| PublishError::Nats(e.to_string()))?
        } else {
            async_nats::connect(url)
                .await
                .map_err(|e| PublishError::Nats(e.to_string()))?
        };

        Ok(Self {
            client,
            subject_prefix: subject_prefix.unwrap_or("huly").to_string(),
        })
    }

    pub fn subject(&self, event_type: &str) -> String {
        subject_for_event(&self.subject_prefix, event_type)
    }

    pub fn is_connected(&self) -> bool {
        self.client.connection_state()
            == async_nats::connection::State::Connected
    }

    /// Returns a reference to the underlying NATS client for reuse by other components.
    pub fn client(&self) -> &async_nats::Client {
        &self.client
    }
}

#[async_trait]
impl EventPublisher for NatsPublisher {
    async fn publish(&self, subject: &str, payload: &[u8]) -> Result<(), PublishError> {
        debug!(subject, payload_len = payload.len(), "publishing to NATS");

        self.client
            .publish(subject.to_string(), payload.to_vec().into())
            .await
            .map_err(|e| {
                error!(subject, error = %e, "NATS publish failed");
                PublishError::Nats(e.to_string())
            })
    }

    async fn flush(&self) -> Result<(), PublishError> {
        self.client
            .flush()
            .await
            .map_err(|e| PublishError::Nats(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subject_for_event_default_prefix() {
        assert_eq!(subject_for_event("huly", "tx"), "huly.event.tx");
    }

    #[test]
    fn subject_for_event_custom_prefix() {
        assert_eq!(
            subject_for_event("myapp", "doc.created"),
            "myapp.event.doc.created"
        );
    }

    #[test]
    fn subject_for_event_dotted_event_type() {
        assert_eq!(
            subject_for_event("huly", "chunter.message"),
            "huly.event.chunter.message"
        );
    }

    #[tokio::test]
    async fn connect_fails_with_invalid_url() {
        let result = NatsPublisher::connect("nats://127.0.0.1:1", None, None).await;
        assert!(matches!(result.unwrap_err(), PublishError::Nats(_)));
    }

    #[tokio::test]
    async fn connect_fails_with_missing_credentials_file() {
        let result = NatsPublisher::connect(
            "nats://127.0.0.1:4222",
            None,
            Some("/nonexistent/path/creds.txt"),
        )
        .await;
        let err = result.unwrap_err();
        assert!(matches!(err, PublishError::Nats(_)));
        assert!(err.to_string().contains("failed to load credentials"));
    }

    #[test]
    fn publish_error_transient_classification() {
        assert!(PublishError::Nats("timeout".into()).is_transient());
        assert!(!PublishError::Serialization("bad".into()).is_transient());
    }

    #[tokio::test]
    async fn mock_publisher_records_calls() {
        let mut mock = MockEventPublisher::new();
        mock.expect_publish()
            .withf(|subject, payload| {
                subject == "huly.event.tx" && !payload.is_empty()
            })
            .times(1)
            .returning(|_, _| Box::pin(async { Ok(()) }));

        mock.publish("huly.event.tx", b"test payload")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn mock_publisher_simulates_error() {
        let mut mock = MockEventPublisher::new();
        mock.expect_publish()
            .returning(|_, _| {
                Box::pin(async { Err(PublishError::Nats("connection refused".into())) })
            });

        let result = mock.publish("huly.event.tx", b"data").await;
        assert!(result.is_err());
    }

    /// Proves the credentials file is read and parsed from disk successfully.
    ///
    /// Strategy: write a syntactically valid `.creds` blob to a temp file, then
    /// call `connect` against an unreachable NATS address.  The file-load step
    /// inside `async_nats::ConnectOptions::with_credentials_file` must succeed
    /// (jwt + nkey parsed, KeyPair created) before the TCP dial even starts, so:
    ///
    ///  * If the error message contains "failed to load credentials" → the file
    ///    path was wrong or the blob was rejected by the parser → test fails.
    ///  * If the error message does NOT contain that phrase → parse succeeded,
    ///    only the network dial failed → test passes.
    #[tokio::test]
    async fn credentials_file_happy_path_loads_from_disk() {
        // Minimal valid NATS creds blob (JWT + seed taken from async-nats's own
        // test fixture; both must satisfy the parser but need no live server).
        const CREDS: &str = "-----BEGIN NATS USER JWT-----\n\
eyJ0eXAiOiJKV1QiLCJhbGciOiJlZDI1NTE5LW5rZXkifQ.eyJqdGkiOiJMN1dBT1hJU0tPSUZNM\
1QyNEhMQ09ENzJRT1czQkNVWEdETjRKVU1SSUtHTlQ3RzdZVFRRIiwiaWF0IjoxNjUxNzkwOTgyLCJ\
pc3MiOiJBRFRRUzdaQ0ZWSk5XNTcyNkdPWVhXNVRTQ1pGTklRU0hLMlpHWVVCQ0Q1RDc3T1ROTE9P\
S1pPWiIsIm5hbWUiOiJUZXN0VXNlciIsInN1YiI6IlVBRkhHNkZVRDJVVTRTREZWQUZVTDVMREZPMl\
hNNFdZTTc2VU5YVFBKWUpLN0VFTVlSQkhUMlZFIiwibmF0cyI6eyJwdWIiOnt9LCJzdWIiOnt9LCJz\
dWJzIjotMSwiZGF0YSI6LTEsInBheWxvYWQiOi0xLCJ0eXBlIjoidXNlciIsInZlcnNpb24iOjJ9fQ\
.bp2-Jsy33l4ayF7Ku1MNdJby4WiMKUrG-rSVYGBusAtV3xP4EdCa-zhSNUaBVIL3uYPPCQYCEoM1p\
CUdOnoJBg\n\
------END NATS USER JWT------\n\
\n\
-----BEGIN USER NKEY SEED-----\n\
SUACH75SWCM5D2JMJM6EKLR2WDARVGZT4QC6LX3AGHSWOMVAKERABBBRWM\n\
------END USER NKEY SEED------\n";

        // Write to a unique temp path; manual cleanup on drop via a guard.
        let creds_path = std::env::temp_dir()
            .join(format!("huly-bridge-test-creds-{}.creds", std::process::id()));
        std::fs::write(&creds_path, CREDS).expect("wrote temp creds file");
        // Ensure cleanup even on panic.
        struct CleanUp(std::path::PathBuf);
        impl Drop for CleanUp {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(&self.0);
            }
        }
        let _guard = CleanUp(creds_path.clone());

        let result = NatsPublisher::connect(
            "nats://127.0.0.1:1",
            None,
            Some(creds_path.to_str().unwrap()),
        )
        .await;

        let err = result.unwrap_err();
        // The error must NOT mention credential loading — that step succeeded.
        // It must be a network/connection failure instead.
        assert!(
            !err.to_string().contains("failed to load credentials"),
            "expected a network error but got: {err}"
        );
        assert!(matches!(err, PublishError::Nats(_)));
    }
}
