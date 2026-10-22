use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use codex_zectrix_dashboard::platform::{Shutdown, application_data_dir};

#[test]
fn explicit_data_directory_wins_on_every_platform() {
    assert_eq!(
        application_data_dir(Some(Path::new("D:/note4")), None, None).unwrap(),
        PathBuf::from("D:/note4")
    );
}

#[test]
fn an_elapsed_deadline_does_not_block_shutdown_waiting() {
    let shutdown = Shutdown::for_test();
    assert!(!shutdown.wait_until(Instant::now() - Duration::from_millis(1)));
}
