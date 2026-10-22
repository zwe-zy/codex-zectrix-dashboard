#![cfg(windows)]
use codex_zectrix_dashboard::codex_discovery::CodexLocator;
use std::{
    fs,
    path::{Path, PathBuf},
};
fn installed(root: &Path, version: &str) -> PathBuf {
    let exe = root
        .join("OpenAI/Codex/bin")
        .join(version)
        .join("codex.exe");
    fs::create_dir_all(exe.parent().unwrap()).unwrap();
    fs::write(&exe, b"fixture").unwrap();
    exe
}
#[test]
fn codex_update_rediscovers_only_automatic_path() {
    let dir = tempfile::tempdir().unwrap();
    let old = installed(dir.path(), "old");
    let automatic = CodexLocator::new(None, Some(dir.path().into()), vec![]);
    let explicit = CodexLocator::new(Some(old.clone()), Some(dir.path().into()), vec![]);
    assert_eq!(automatic.resolve().unwrap().path, old);
    fs::remove_file(&old).unwrap();
    let new = installed(dir.path(), "new");
    assert_eq!(automatic.resolve().unwrap().path, new);
    assert!(explicit.resolve().is_err());
}
#[test]
fn desktop_only_install_does_not_need_path() {
    let dir = tempfile::tempdir().unwrap();
    let exe = installed(dir.path(), "desktop");
    let resolved = CodexLocator::new(None, Some(dir.path().into()), vec![])
        .resolve()
        .unwrap();
    assert_eq!(resolved.path, exe);
    assert!(!resolved.explicit);
}
#[cfg(feature = "test-support")]
#[test]
fn automatic_discovery_tries_a_working_candidate_after_startup_failure() {
    let dir = tempfile::tempdir().unwrap();
    installed(dir.path(), "broken");
    let path = dir.path().join("working-path");
    fs::create_dir(&path).unwrap();
    fs::copy(
        env!("CARGO_BIN_EXE_codex-zectrix-test-app-server"),
        path.join("codex.exe"),
    )
    .unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_codex-zectrix-dashboard"))
        .arg("diagnostics")
        .env_remove("CODEX_ZECTRIX_CODEX_BIN")
        .env("LOCALAPPDATA", dir.path())
        .env("PATH", path)
        .env("CODEX_ZECTRIX_DATA_DIR", dir.path().join("data"))
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("codex_quota_method=supported\n")
    );
}
