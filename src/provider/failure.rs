//! Sanitized failures retain their retry classification across provider boundaries.
use std::borrow::Cow;

/// Deliberately contains no raw socket, response, API key, or server error text.
#[derive(Debug)]
pub(super) struct Failure {
    pub(super) message: Cow<'static, str>,
    pub(super) retryable: bool,
    pub(super) needs_transcription_context: bool,
}

impl std::fmt::Display for Failure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}
impl std::error::Error for Failure {}

impl Failure {
    pub(super) fn http(service: &'static str, status: u16) -> Self {
        Self {
            message: Cow::Owned(format!("{service} returned HTTP {status}")),
            retryable: matches!(status, 408 | 425 | 429 | 500..=599),
            needs_transcription_context: false,
        }
    }
    pub(super) fn fatal(message: &'static str) -> Self {
        Self {
            message: Cow::Borrowed(message),
            retryable: false,
            needs_transcription_context: false,
        }
    }
    pub(super) fn retry(message: &'static str) -> Self {
        Self {
            message: Cow::Borrowed(message),
            retryable: true,
            needs_transcription_context: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn http_credentials_and_model_errors_pause_while_service_outages_can_retry() {
        for status in [400, 401, 403, 404, 413, 422] {
            let error = anyhow::Error::new(Failure::http("whisper.cpp", status))
                .context("Original recognition");
            assert!(!crate::provider::retryable_error(&error));
        }
        for status in [408, 429, 500, 502, 503, 504] {
            let error = anyhow::Error::new(Failure::http("whisper.cpp", status))
                .context("Original recognition");
            assert!(crate::provider::retryable_error(&error));
        }
    }
}
