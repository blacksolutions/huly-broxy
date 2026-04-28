use metrics::{counter, gauge};
use metrics_exporter_prometheus::PrometheusBuilder;
use tracing::info;

/// Initialize Prometheus metrics exporter and return the handle for the /metrics endpoint
pub fn init_metrics() -> Result<metrics_exporter_prometheus::PrometheusHandle, MetricsError> {
    let builder = PrometheusBuilder::new();
    let handle = builder
        .install_recorder()
        .map_err(|e| MetricsError::Init(e.to_string()))?;

    // Register initial metrics with zero values
    counter!("huly_bridge_events_forwarded_total").absolute(0);
    counter!("huly_bridge_events_failed_total").absolute(0);
    counter!("huly_bridge_events_dropped_total").absolute(0);
    counter!("huly_bridge_ws_reconnects_total").absolute(0);
    counter!("huly_bridge_pending_requests_dropped_total").absolute(0);
    gauge!("huly_bridge_ws_connected").set(0.0);
    gauge!("huly_bridge_nats_connected").set(0.0);

    info!("prometheus metrics initialized");
    Ok(handle)
}

pub fn record_event_forwarded() {
    counter!("huly_bridge_events_forwarded_total").increment(1);
}

pub fn record_event_failed() {
    counter!("huly_bridge_events_failed_total").increment(1);
}

pub fn record_event_dropped() {
    counter!("huly_bridge_events_dropped_total").increment(1);
}

pub fn record_ws_reconnect() {
    counter!("huly_bridge_ws_reconnects_total").increment(1);
}

pub fn record_pending_request_dropped() {
    counter!("huly_bridge_pending_requests_dropped_total").increment(1);
}

pub fn set_ws_connected(connected: bool) {
    gauge!("huly_bridge_ws_connected").set(if connected { 1.0 } else { 0.0 });
}

pub fn set_nats_connected(connected: bool) {
    gauge!("huly_bridge_nats_connected").set(if connected { 1.0 } else { 0.0 });
}

#[derive(Debug, thiserror::Error)]
pub enum MetricsError {
    #[error("failed to initialize metrics: {0}")]
    Init(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use metrics::with_local_recorder;
    use metrics_exporter_prometheus::PrometheusBuilder;

    #[test]
    fn record_event_forwarded_increments() {
        let recorder = PrometheusBuilder::new().build_recorder();
        let handle = recorder.handle();
        with_local_recorder(&recorder, || {
            record_event_forwarded();
            record_event_forwarded();
            record_event_forwarded();
        });
        let output = handle.render();
        assert!(
            output.contains("huly_bridge_events_forwarded_total 3"),
            "expected counter=3 in:\n{output}"
        );
    }

    #[test]
    fn record_event_failed_increments() {
        let recorder = PrometheusBuilder::new().build_recorder();
        let handle = recorder.handle();
        with_local_recorder(&recorder, || {
            record_event_failed();
            record_event_failed();
        });
        let output = handle.render();
        assert!(
            output.contains("huly_bridge_events_failed_total 2"),
            "expected counter=2 in:\n{output}"
        );
    }

    #[test]
    fn record_ws_reconnect_increments() {
        let recorder = PrometheusBuilder::new().build_recorder();
        let handle = recorder.handle();
        with_local_recorder(&recorder, || {
            record_ws_reconnect();
        });
        let output = handle.render();
        assert!(
            output.contains("huly_bridge_ws_reconnects_total 1"),
            "expected counter=1 in:\n{output}"
        );
    }

    #[test]
    fn record_pending_request_dropped_increments() {
        let recorder = PrometheusBuilder::new().build_recorder();
        let handle = recorder.handle();
        with_local_recorder(&recorder, || {
            record_pending_request_dropped();
            record_pending_request_dropped();
        });
        let output = handle.render();
        assert!(
            output.contains("huly_bridge_pending_requests_dropped_total 2"),
            "expected counter=2 in:\n{output}"
        );
    }

    #[test]
    fn record_event_dropped_increments() {
        let recorder = PrometheusBuilder::new().build_recorder();
        let handle = recorder.handle();
        with_local_recorder(&recorder, || {
            record_event_dropped();
        });
        let output = handle.render();
        assert!(
            output.contains("huly_bridge_events_dropped_total 1"),
            "expected counter=1 in:\n{output}"
        );
    }

    #[test]
    fn set_ws_connected_toggles_gauge() {
        let recorder = PrometheusBuilder::new().build_recorder();
        let handle = recorder.handle();
        with_local_recorder(&recorder, || {
            set_ws_connected(true);
        });
        let output = handle.render();
        assert!(
            output.contains("huly_bridge_ws_connected 1"),
            "expected gauge=1 in:\n{output}"
        );

        with_local_recorder(&recorder, || {
            set_ws_connected(false);
        });
        let output = handle.render();
        assert!(
            output.contains("huly_bridge_ws_connected 0"),
            "expected gauge=0 in:\n{output}"
        );
    }

    #[test]
    fn set_nats_connected_toggles_gauge() {
        let recorder = PrometheusBuilder::new().build_recorder();
        let handle = recorder.handle();
        with_local_recorder(&recorder, || {
            set_nats_connected(true);
        });
        let output = handle.render();
        assert!(
            output.contains("huly_bridge_nats_connected 1"),
            "expected gauge=1 in:\n{output}"
        );

        with_local_recorder(&recorder, || {
            set_nats_connected(false);
        });
        let output = handle.render();
        assert!(
            output.contains("huly_bridge_nats_connected 0"),
            "expected gauge=0 in:\n{output}"
        );
    }
}
