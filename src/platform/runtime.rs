use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use super::PlatformError;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LocalParts {
    pub month: u8,
    pub day: u8,
    pub weekday_from_sunday: u8,
    pub hour: u8,
    pub minute: u8,
}

pub fn now_epoch_seconds() -> Result<i64, PlatformError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)?
        .as_secs()
        .try_into()
        .map_err(|_| PlatformError::EpochRange)
}

pub fn local_parts(epoch_seconds: i64) -> Result<LocalParts, PlatformError> {
    let offset = time::UtcOffset::current_local_offset()?;
    let datetime = time::OffsetDateTime::from_unix_timestamp(epoch_seconds)?.to_offset(offset);
    Ok(LocalParts {
        month: datetime.month() as u8,
        day: datetime.day(),
        weekday_from_sunday: datetime.weekday().number_days_from_sunday(),
        hour: datetime.hour(),
        minute: datetime.minute(),
    })
}

pub struct Shutdown {
    receiver: Receiver<()>,
}

impl Shutdown {
    pub fn install() -> Result<Self, PlatformError> {
        let (sender, receiver) = mpsc::channel();
        ctrlc::set_handler(move || {
            let _ = sender.send(());
        })?;
        Ok(Self { receiver })
    }

    pub fn for_test() -> Self {
        let (_sender, receiver) = mpsc::channel();
        Self { receiver }
    }

    pub fn wait_until(&self, deadline: Instant) -> bool {
        match self
            .receiver
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        {
            Ok(()) => true,
            Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => false,
        }
    }
}
