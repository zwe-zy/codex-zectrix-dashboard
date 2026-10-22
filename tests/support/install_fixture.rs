use codex_zectrix_dashboard::install::{HostSnapshot, InstallError, InstallHost, PackageManifest};
use codex_zectrix_dashboard::platform::background_windows::OperationGuard;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

#[derive(Default)]
pub struct Host {
    pub state: HostSnapshot,
    pub deny_stop: bool,
    pub fail_ready: bool,
    pub starts: usize,
    pub versions_checked: usize,
}
impl InstallHost for Host {
    fn snapshot(&mut self) -> Result<HostSnapshot, InstallError> {
        Ok(self.state.clone())
    }
    fn verify_binaries(&mut self, _: &Path, _: &str) -> Result<(), InstallError> {
        self.versions_checked += 1;
        Ok(())
    }
    fn quiesce(&mut self, _: &HostSnapshot) -> Result<(), InstallError> {
        if self.deny_stop {
            Err(InstallError::LegacyStop)
        } else {
            self.state.running_executable = None;
            Ok(())
        }
    }
    fn activate(
        &mut self,
        exe: &Path,
        previous: &HostSnapshot,
        _: &OperationGuard,
    ) -> Result<(), InstallError> {
        if previous.startup.is_some() {
            self.state.startup = Some(format!("\"{}\"", exe.display()));
        }
        if self.fail_ready {
            return Err(InstallError::Health);
        }
        if previous.running_executable.is_some() {
            self.starts += 1;
            self.state.running_executable = Some(exe.into());
        }
        Ok(())
    }
    fn restore(&mut self, previous: &HostSnapshot, _: &OperationGuard) -> Result<(), InstallError> {
        self.state = previous.clone();
        Ok(())
    }
}
pub fn package(base: &Path, version: &str) -> PathBuf {
    let path = base.join(format!("package-{version}"));
    fs::create_dir_all(&path).unwrap();
    let mut files = BTreeMap::new();
    for name in [
        "codex-zectrix-dashboard.exe",
        "codex-zectrix-companion.exe",
        "LICENSE",
        "快速入门.txt",
    ] {
        let bytes = format!("fixture {version} {name}");
        fs::write(path.join(name), &bytes).unwrap();
        files.insert(
            name.to_string(),
            format!("{:x}", Sha256::digest(bytes.as_bytes())),
        );
    }
    let manifest = PackageManifest {
        schema_version: 1,
        version: version.into(),
        platform: "windows-x86_64".into(),
        files,
    };
    fs::write(
        path.join("package.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    path
}

#[allow(dead_code)]
pub fn desktop_package(base: &Path, version: &str) -> PathBuf {
    let path = package(base, version);
    let mut manifest: PackageManifest =
        serde_json::from_slice(&fs::read(path.join("package.json")).unwrap()).unwrap();
    manifest.schema_version = 2;
    for name in ["NOTE4控制台.exe", "THIRD-PARTY-NOTICES.txt"] {
        let bytes = format!("desktop fixture {version} {name}");
        fs::write(path.join(name), &bytes).unwrap();
        manifest.files.insert(
            name.into(),
            format!("{:x}", Sha256::digest(bytes.as_bytes())),
        );
    }
    fs::write(
        path.join("package.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    path
}
