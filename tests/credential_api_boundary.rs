use std::fs;
use std::process::Command;

#[test]
fn downstream_consumers_cannot_import_the_memory_credential_store() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let fixture = root.join("tests/fixtures/memory_credential_consumer.rs");
    let dependencies = root.join("target/debug/deps");
    let library = fs::read_dir(&dependencies)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            name.starts_with("libcodex_zectrix_dashboard-") && name.ends_with(".rlib")
        })
        .max_by_key(|entry| entry.metadata().unwrap().modified().unwrap())
        .unwrap()
        .path();
    let target = tempfile::tempdir().unwrap();
    let cargo = std::path::Path::new(env!("CARGO"));
    let rustc = cargo
        .parent()
        .map(|directory| directory.join(if cfg!(windows) { "rustc.exe" } else { "rustc" }))
        .unwrap_or_else(|| "rustc".into());
    let output = Command::new(rustc)
        .current_dir(root)
        .args([
            "--edition=2024",
            "--emit=metadata",
            fixture.to_str().unwrap(),
            "--extern",
            &format!("codex_zectrix_dashboard={}", library.to_str().unwrap()),
            "-L",
            &format!("dependency={}", dependencies.to_str().unwrap()),
            "--out-dir",
            target.path().to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(
        !output.status.success(),
        "downstream consumer unexpectedly imported MemoryCredentialStore"
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("MemoryCredentialStore"), "{stderr}");
    assert!(
        stderr.contains("no `MemoryCredentialStore` in the root"),
        "{stderr}"
    );
}
