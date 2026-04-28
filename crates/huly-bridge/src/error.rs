use thiserror::Error;

#[derive(Debug, Error)]
pub enum BridgeError {
    #[error("huly auth failed: {0}")]
    AuthFailed(String),

    #[error("huly connection lost: {0}")]
    ConnectionLost(String),

    #[error("rpc error from huly: method={method}, code={code}, message={message}")]
    RpcError {
        method: String,
        code: String,
        message: String,
    },

    #[error("nats publish failed: {0}")]
    NatsPublish(String),

    #[error("config error: {0}")]
    Config(String),

    #[error("serialization error: {0}")]
    Serialization(String),

    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl BridgeError {
    pub fn is_transient(&self) -> bool {
        matches!(
            self,
            BridgeError::ConnectionLost(_) | BridgeError::NatsPublish(_)
        )
    }

    pub fn is_fatal(&self) -> bool {
        matches!(
            self,
            BridgeError::AuthFailed(_) | BridgeError::Config(_)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transient_errors_classified_correctly() {
        let err = BridgeError::ConnectionLost("ws closed".into());
        assert!(err.is_transient());
        assert!(!err.is_fatal());

        let err = BridgeError::NatsPublish("timeout".into());
        assert!(err.is_transient());
    }

    #[test]
    fn fatal_errors_classified_correctly() {
        let err = BridgeError::AuthFailed("bad token".into());
        assert!(err.is_fatal());
        assert!(!err.is_transient());

        let err = BridgeError::Config("missing field".into());
        assert!(err.is_fatal());
    }

    #[test]
    fn error_display_formatting() {
        let err = BridgeError::RpcError {
            method: "findAll".into(),
            code: "404".into(),
            message: "not found".into(),
        };
        assert_eq!(
            err.to_string(),
            "rpc error from huly: method=findAll, code=404, message=not found"
        );
    }
}
