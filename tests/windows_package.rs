#![cfg(windows)]
#[path = "support/install_fixture.rs"]
#[allow(dead_code)]
mod fixture;
use codex_zectrix_dashboard::install::{Package, PackageManifest};
use sha2::{Digest, Sha256};
use std::{fs, path::Path, process::Command};

#[test]
fn package_rejects_missing_gui_and_extra_files() {
    let dir = tempfile::tempdir().unwrap();
    let package = fixture::desktop_package(dir.path(), "0.3.0-rc.1");
    assert!(Package::load(&package).is_ok());
    fs::write(package.join("private.txt"), b"keep out").unwrap();
    assert!(Package::load(&package).is_err());
    fs::remove_file(package.join("private.txt")).unwrap();
    let gui = package.join("NOTE4控制台.exe");
    let original = fs::read(&gui).unwrap();
    fs::write(&gui, b"tampered").unwrap();
    assert!(Package::load(&package).is_err());
    fs::write(&gui, original).unwrap();
    fs::remove_file(gui).unwrap();
    assert!(Package::load(&package).is_err());
}

fn real_package(base: &Path, version: &str) -> std::path::PathBuf {
    let path = fixture::package(base, version);
    let mut manifest: PackageManifest =
        serde_json::from_slice(&fs::read(path.join("package.json")).unwrap()).unwrap();
    for (name, source) in [
        (
            "codex-zectrix-dashboard.exe",
            env!("CARGO_BIN_EXE_codex-zectrix-dashboard"),
        ),
        (
            "codex-zectrix-companion.exe",
            env!("CARGO_BIN_EXE_codex-zectrix-companion"),
        ),
    ] {
        fs::copy(source, path.join(name)).unwrap();
        manifest.files.insert(
            name.into(),
            format!("{:x}", Sha256::digest(fs::read(source).unwrap())),
        );
    }
    fs::write(
        path.join("package.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    path
}
#[test]
fn packaged_preview_runs_from_unicode_path_and_verifies_paired_versions() {
    let dir = tempfile::tempdir().unwrap();
    let unicode = dir.path().join("中文路径 with spaces & test");
    fs::create_dir(&unicode).unwrap();
    let package = real_package(&unicode, env!("CARGO_PKG_VERSION"));
    let exe = package.join("codex-zectrix-dashboard.exe");
    let verification = Command::new(&exe)
        .arg("verify-package")
        .arg(&package)
        .output()
        .unwrap();
    assert!(
        verification.status.success(),
        "{}",
        String::from_utf8_lossy(&verification.stderr)
    );
    let png = unicode.join("预览.png");
    let data = unicode.join("unused-data");
    let result = Command::new(&exe)
        .args(["preview", "--output"])
        .arg(&png)
        .env("CODEX_ZECTRIX_DATA_DIR", &data)
        .output()
        .unwrap();
    assert!(result.status.success());
    assert_eq!(image::image_dimensions(png).unwrap(), (400, 300));
    assert!(!data.exists(), "offline preview must not create user state");
}
#[test]
fn package_rejects_mismatched_companion() {
    let dir = tempfile::tempdir().unwrap();
    // Manifest hashes are valid, but the executable versions do not match its declared version.
    let package = real_package(dir.path(), "0.0.0-mismatched");
    assert!(Package::load(&package).is_ok());
    let result = Command::new(env!("CARGO_BIN_EXE_codex-zectrix-dashboard"))
        .arg("verify-package")
        .arg(&package)
        .output()
        .unwrap();
    assert!(!result.status.success());
}
#[test]
fn package_excludes_local_state_and_rejects_wrong_platform_or_hash() {
    let dir = tempfile::tempdir().unwrap();
    let package = fixture::package(dir.path(), "0.2.0-rc.1");
    assert!(Package::load(&package).is_ok());
    for name in ["settings.json", "companion.log", "credentials.txt"] {
        fs::write(package.join(name), "private sentinel").unwrap();
        assert!(Package::load(&package).is_err());
        fs::remove_file(package.join(name)).unwrap();
    }
    let original = fs::read(package.join("package.json")).unwrap();
    let mut manifest: PackageManifest = serde_json::from_slice(&original).unwrap();
    manifest.platform = "linux-x86_64".into();
    fs::write(
        package.join("package.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    assert!(Package::load(&package).is_err());
    fs::write(package.join("package.json"), original).unwrap();
    fs::write(
        package.join("codex-zectrix-companion.exe"),
        b"different binary",
    )
    .unwrap();
    assert!(Package::load(&package).is_err());
    fs::remove_file(package.join("codex-zectrix-companion.exe")).unwrap();
    assert!(Package::load(&package).is_err());
}
