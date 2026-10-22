#[cfg(windows)]
pub mod background_windows;
mod paths;
mod process;
#[cfg(windows)]
pub(crate) mod registry_windows;
mod runtime;
#[cfg(windows)]
pub mod shortcut_windows;

pub use paths::{application_data_dir, data_dir};
pub use process::{OwnedProcess, ProcessError};
pub use runtime::{LocalParts, Shutdown, local_parts, now_epoch_seconds};

#[derive(Debug, thiserror::Error)]
pub enum PlatformError {
    #[error("无法确定应用数据目录")]
    MissingDataDirectory,
    #[error("UnsupportedPlatform")]
    UnsupportedPlatform,
    #[error("系统时间早于 Unix epoch：{0}")]
    SystemTime(#[from] std::time::SystemTimeError),
    #[error("Unix 时间戳超出支持范围：{0}")]
    Timestamp(#[from] time::error::ComponentRange),
    #[error("无法确定本地时区偏移：{0}")]
    LocalOffset(#[from] time::error::IndeterminateOffset),
    #[error("无法安装终止信号处理器：{0}")]
    ShutdownHandler(#[from] ctrlc::Error),
    #[error("当前秒数超出 i64 范围")]
    EpochRange,
}
