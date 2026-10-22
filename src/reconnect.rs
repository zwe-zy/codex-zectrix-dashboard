use std::time::Duration;

const RECONNECT_DELAYS: [Duration; 4] = [
    Duration::from_secs(5),
    Duration::from_secs(30),
    Duration::from_secs(5 * 60),
    Duration::from_secs(60 * 60),
];
const HEALTHY_RESET_AFTER: Duration = Duration::from_secs(10 * 60);

#[derive(Clone, Debug, Default)]
pub struct ReconnectPolicy {
    consecutive_failures: usize,
    healthy_since: Option<Duration>,
    reconnect_at: Option<Duration>,
}

impl ReconnectPolicy {
    pub fn record_transport_failure(&mut self, now: Duration) -> Duration {
        self.healthy_since = None;
        let delay = RECONNECT_DELAYS[self.consecutive_failures.min(RECONNECT_DELAYS.len() - 1)];
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        self.reconnect_at = Some(now.saturating_add(delay));
        delay
    }

    pub fn record_session_started(&mut self, now: Duration) {
        self.healthy_since = Some(now);
        self.reconnect_at = None;
    }

    pub fn record_transport_healthy(&mut self, now: Duration) {
        if self
            .healthy_since
            .is_some_and(|started| now.saturating_sub(started) >= HEALTHY_RESET_AFTER)
        {
            self.consecutive_failures = 0;
        }
    }

    pub fn retry_after(&self, now: Duration) -> Option<Duration> {
        self.reconnect_at
            .and_then(|reconnect_at| reconnect_at.checked_sub(now))
            .filter(|remaining| !remaining.is_zero())
    }
}
