//! Parses Huly REST rate-limit response headers (added in 0.7.19).
//!
//! Extracted from `bridge::proxy` so the parsing rules live in one place and
//! can be unit-tested in isolation. The transactor exposes:
//!
//! - `X-RateLimit-Limit`     → `limit`         (max requests in window)
//! - `X-RateLimit-Remaining` → `remaining`     (remaining quota)
//! - `X-RateLimit-Reset`     → `reset_ms`      (epoch ms when window resets)
//! - `Retry-After-ms`        → `retry_after_ms` (preferred precision)
//! - `Retry-After`           → `retry_after_ms` (seconds; only if `-ms` absent)
//!
//! Every field is optional — older transactors emit none of these. Callers get
//! the raw data only; deciding what to do (back off, surface to admin, etc.)
//! is out of scope for this module.

use reqwest::header::HeaderMap;
use serde::Serialize;

/// Structured view of the rate-limit headers attached to a single REST response.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct RateLimitInfo {
    /// Total number of requests allowed in the current window.
    pub limit: Option<u64>,
    /// Requests remaining in the current window.
    pub remaining: Option<u64>,
    /// Epoch milliseconds at which the window resets.
    pub reset_ms: Option<u64>,
    /// Suggested wait, in milliseconds, before the client retries.
    pub retry_after_ms: Option<u64>,
}

impl RateLimitInfo {
    /// Parse all rate-limit fields from a response header map.
    ///
    /// Unknown or unparseable header values are silently dropped to `None` —
    /// upstream control over header formatting is poor and we never want a
    /// malformed header to break an otherwise-successful request.
    pub fn from_headers(headers: &HeaderMap) -> Self {
        let limit = parse_u64_header(headers, "X-RateLimit-Limit");
        let remaining = parse_u64_header(headers, "X-RateLimit-Remaining");
        let reset_ms = parse_u64_header(headers, "X-RateLimit-Reset");

        // Prefer the millisecond-precision header; fall back to the
        // standard seconds header (RFC 7231) only when it's absent.
        let retry_after_ms = parse_u64_header(headers, "Retry-After-ms").or_else(|| {
            parse_u64_header(headers, "Retry-After").map(|secs| secs.saturating_mul(1000))
        });

        Self { limit, remaining, reset_ms, retry_after_ms }
    }

    /// `true` when no rate-limit headers were present.
    pub fn is_empty(&self) -> bool {
        self.limit.is_none()
            && self.remaining.is_none()
            && self.reset_ms.is_none()
            && self.retry_after_ms.is_none()
    }
}

fn parse_u64_header(headers: &HeaderMap, name: &str) -> Option<u64> {
    headers.get(name)?.to_str().ok()?.trim().parse::<u64>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_headers_yield_empty_info() {
        let info = RateLimitInfo::from_headers(&HeaderMap::new());
        assert!(info.is_empty());
    }

    #[test]
    fn parses_all_fields() {
        let mut headers = HeaderMap::new();
        headers.insert("X-RateLimit-Limit", "100".parse().unwrap());
        headers.insert("X-RateLimit-Remaining", "37".parse().unwrap());
        headers.insert("X-RateLimit-Reset", "1700000000000".parse().unwrap());
        headers.insert("Retry-After-ms", "2500".parse().unwrap());

        let info = RateLimitInfo::from_headers(&headers);
        assert_eq!(info.limit, Some(100));
        assert_eq!(info.remaining, Some(37));
        assert_eq!(info.reset_ms, Some(1700000000000));
        assert_eq!(info.retry_after_ms, Some(2500));
        assert!(!info.is_empty());
    }

    #[test]
    fn retry_after_seconds_used_when_ms_absent() {
        let mut headers = HeaderMap::new();
        headers.insert("Retry-After", "5".parse().unwrap());

        let info = RateLimitInfo::from_headers(&headers);
        assert_eq!(info.retry_after_ms, Some(5_000));
    }

    #[test]
    fn ms_header_wins_over_seconds_header() {
        let mut headers = HeaderMap::new();
        headers.insert("Retry-After-ms", "750".parse().unwrap());
        headers.insert("Retry-After", "10".parse().unwrap());

        let info = RateLimitInfo::from_headers(&headers);
        assert_eq!(info.retry_after_ms, Some(750));
    }

    #[test]
    fn malformed_values_are_ignored() {
        let mut headers = HeaderMap::new();
        headers.insert("X-RateLimit-Limit", "garbage".parse().unwrap());
        headers.insert("Retry-After", "soon".parse().unwrap());

        let info = RateLimitInfo::from_headers(&headers);
        assert!(info.limit.is_none());
        assert!(info.retry_after_ms.is_none());
    }
}
