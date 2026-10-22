use std::time::Duration;

pub const TASK_STALE_AFTER: Duration = Duration::from_secs(180);
pub const QUOTA_STALE_AFTER: Duration = Duration::from_secs(1_800);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FreshValue<T> {
    pub value: Option<T>,
    pub last_success_epoch_seconds: Option<i64>,
}

impl<T> Default for FreshValue<T> {
    fn default() -> Self {
        Self {
            value: None,
            last_success_epoch_seconds: None,
        }
    }
}

impl<T> FreshValue<T> {
    pub fn with_success(value: T, epoch_seconds: i64) -> Self {
        Self {
            value: Some(value),
            last_success_epoch_seconds: Some(epoch_seconds),
        }
    }

    pub fn record_success(&mut self, value: T, epoch_seconds: i64) {
        self.value = Some(value);
        self.last_success_epoch_seconds = Some(epoch_seconds);
    }

    pub fn is_stale(&self, now_epoch_seconds: i64, threshold: Duration) -> bool {
        let (Some(_), Some(last_success)) = (&self.value, self.last_success_epoch_seconds) else {
            return false;
        };
        let Ok(threshold_seconds) = i64::try_from(threshold.as_secs()) else {
            return false;
        };
        now_epoch_seconds < last_success
            || now_epoch_seconds.saturating_sub(last_success) >= threshold_seconds
    }
}
