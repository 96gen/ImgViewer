use std::time::{Duration, Instant};

use imgviewer_codec_protocol::CODEC_HELPER_DECODE_DEADLINE_MS;

pub(crate) const DEFAULT_DECODE_TIMEOUT: Duration =
    Duration::from_millis(CODEC_HELPER_DECODE_DEADLINE_MS);

#[derive(Clone, Copy, Debug)]
pub(crate) struct DecodePolicy {
    max_decode_duration: Duration,
}

impl Default for DecodePolicy {
    fn default() -> Self {
        Self {
            max_decode_duration: DEFAULT_DECODE_TIMEOUT,
        }
    }
}

impl DecodePolicy {
    #[cfg(test)]
    pub(crate) fn with_max_decode_duration(max_decode_duration: Duration) -> Self {
        Self {
            max_decode_duration,
        }
    }

    pub(crate) fn deadline_from(self, started_at: Instant) -> DecodeDeadline {
        DecodeDeadline {
            expires_at: started_at
                .checked_add(self.max_decode_duration)
                .unwrap_or(started_at),
            limit_ms: self
                .max_decode_duration
                .as_millis()
                .try_into()
                .unwrap_or(u64::MAX),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct DecodeDeadline {
    expires_at: Instant,
    limit_ms: u64,
}

impl DecodeDeadline {
    pub(crate) fn is_expired(self, now: Instant) -> bool {
        now >= self.expires_at
    }

    pub(crate) fn limit_ms(self) -> u64 {
        self.limit_ms
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_deadline_has_a_stable_limit_and_boundary() {
        let started = Instant::now();
        let deadline = DecodePolicy::with_max_decode_duration(Duration::from_millis(25))
            .deadline_from(started);
        assert_eq!(deadline.limit_ms(), 25);
        assert!(!deadline.is_expired(started + Duration::from_millis(24)));
        assert!(deadline.is_expired(started + Duration::from_millis(25)));
    }
}
