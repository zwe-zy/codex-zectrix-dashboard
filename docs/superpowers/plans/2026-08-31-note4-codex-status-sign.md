# NOTE4 Codex P-Style Windows Status Sign Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Port the proven ZECTRIX dashboard companion to Windows and deliver a privacy-first 400×300 P-style Codex status sign that shows the latest top-level task state, remaining quota, reset time, and last successful screen update.

**Architecture:** Fork the existing Rust project and keep its persistent `codex app-server`, renderer, ZECTRIX client, changed-frame publisher, and fixture-driven tests. Add small platform modules for Windows paths, shutdown, credential storage, process ownership, and autostart; replace title-based multi-task/radar rendering with one normalized privacy-safe status model and the approved P layout.

**Tech Stack:** Rust 2024, `serde`/`serde_json`, blocking `reqwest` with rustls, `embedded-graphics`, `u8g2-fonts`, `image`, `sha2`, `keyring` 3 with native Apple/Windows stores, Windows Task Scheduler via `schtasks.exe`, Cargo integration tests, GitHub Actions Windows/macOS matrix.

**Spec:** `docs/superpowers/specs/2026-08-31-note4-codex-status-sign-design.md`

## Global Constraints

- Base the implementation on `https://github.com/BarryBarrywu/codex-zectrix-dashboard` version `0.1.9`; preserve its MIT license and upstream history.
- Primary target is Windows with the installed Codex Desktop and `codex app-server`; macOS must continue to compile unless a task explicitly replaces a macOS-only path with a shared implementation.
- Keep NOTE4 on original firmware and publish only through the ZECTRIX Cloud API.
- Render exactly 400×300 pixels as 1-bit black and white.
- Never place task title, task ID, prompt, response, reasoning, tool arguments, plan text, project path, or account identity in the normalized screen model, PNG, cache, or logs.
- Observe the latest top-level task every 60 seconds and quota every 10 minutes; app-server notifications may update earlier.
- Upload only a visibly changed frame. `UPDATED HH:MM` means the last successful changed-frame upload, not a ticking clock.
- Store the ZECTRIX API key in Windows Credential Manager or macOS Keychain; never in settings, cache, logs, command-line arguments, or conversation text.
- Require a local preview and an explicit terminal confirmation immediately before the first physical upload.
- Do not implement reset radar, task control, firmware flashing, task titles, multiple task rows, or a complex desktop GUI.

## Planned File Structure

Work inside a fork named `codex-zectrix-dashboard-windows/` created from the upstream repository.

```text
src/
  app_server.rs                 persistent JSONL RPC client
  autostart.rs                  cross-platform autostart interface
  companion.rs                 observation, freshness, scheduling, publishing
  credentials.rs               secure credential interface and keyring adapter
  codex_status.rs               strict quota/thread/turn parsing and mapping
  platform/
    mod.rs                      shared exports
    paths.rs                    user data directory resolution
    process.rs                  process-owner facade
    process_unix.rs             existing process-group behavior
    process_windows.rs          Windows Job Object behavior
    runtime.rs                  clock, local time, shutdown-aware waiting
  status_board/
    mod.rs                      exported model and renderer entry points
    model.rs                    privacy-safe dashboard model
    render.rs                   approved 400×300 P-style renderer
  setup.rs                      device discovery, preview, confirmation, settings
  settings.rs                   non-secret persisted settings
  publisher.rs                  changed-frame queue, retry, ZECTRIX upload
  lib.rs                        focused public exports
  main.rs                       CLI entry point
  bin/codex-zectrix-companion.rs hidden-window Windows companion entry point
tests/
  app_server_client.rs
  autostart.rs
  codex_status.rs
  credentials.rs
  p_dashboard.rs
  publisher.rs
  setup_cli.rs
  windows_companion.rs
  support/fake_codex.rs
scripts/
  build-release.ps1
  test-clean-install.ps1
docs/
  adr/0010-support-windows-native-runtime.md
  adr/0011-read-current-thread-status-from-app-server.md
  adr/0012-use-private-single-status-p-layout.md
  canary/windows-note4-2026-08-31.md
```

Legacy radar, title correlation, hook recording, and launchd lifecycle files remain until Task 8, so every earlier task keeps the repository buildable.

---

### Task 1: Create the Windows Branch and Platform Runtime Foundation

**Files:**
- Modify: `Cargo.toml`
- Modify: `src/lib.rs`
- Modify: `src/main.rs`
- Modify: `src/setup.rs`
- Create: `src/platform/mod.rs`
- Create: `src/platform/paths.rs`
- Create: `src/platform/runtime.rs`
- Create: `tests/platform.rs`

**Interfaces:**
- Produces: `platform::data_dir() -> Result<PathBuf, PlatformError>`
- Produces: `platform::now_epoch_seconds() -> Result<i64, PlatformError>`
- Produces: `platform::local_parts(i64) -> Result<LocalParts, PlatformError>`
- Produces: `Shutdown::install() -> Result<Shutdown, PlatformError>` and `Shutdown::wait_until(Instant) -> bool`

- [ ] **Step 1: Clone the upstream repository and create the implementation branch**

```powershell
git clone https://github.com/BarryBarrywu/codex-zectrix-dashboard.git codex-zectrix-dashboard-windows
Set-Location codex-zectrix-dashboard-windows
git switch -c feat/windows-note4-status-sign
git status --short
```

Expected: clean branch based on upstream `main`, with `Cargo.toml` version `0.1.9` before the first change.

- [ ] **Step 2: Record the Windows baseline failure**

Run:

```powershell
cargo test --locked --all-features
```

Expected before the port: compilation fails on one or more Unix-only imports such as `std::os::unix`, `libc::localtime_r`, `/bin/ps`, or signal handling. Save the compiler output in the task notes; do not weaken tests to hide it.

- [ ] **Step 3: Write the failing platform contract tests**

```rust
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use codex_zectrix_dashboard::platform::{application_data_dir, Shutdown};

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
```

Run: `cargo test --test platform -- --nocapture`

Expected: FAIL because the `platform` module and interfaces do not exist.

- [ ] **Step 4: Add platform-neutral paths, time, and shutdown-aware waiting**

Use a pure helper for testability and keep environment access at the edge:

```rust
pub fn application_data_dir(
    override_dir: Option<&Path>,
    local_app_data: Option<&Path>,
    home: Option<&Path>,
) -> Result<PathBuf, PlatformError> {
    if let Some(path) = override_dir { return Ok(path.to_owned()); }
    #[cfg(windows)]
    if let Some(path) = local_app_data {
        return Ok(path.join("CodexZectrixDashboard"));
    }
    #[cfg(target_os = "macos")]
    if let Some(path) = home {
        return Ok(path.join("Library/Application Support/codex-zectrix-dashboard"));
    }
    Err(PlatformError::MissingDataDirectory)
}
```

Implement `Shutdown` with `ctrlc::set_handler` and a standard-library channel so `wait_until` wakes immediately on Ctrl+C or termination instead of using `libc::poll`. Enable `time`'s `local-offset` feature and implement `local_parts` without `libc::localtime_r`.

- [ ] **Step 5: Route current call sites through the platform module**

Replace the duplicate `data_dir`, epoch-time, local-time, `signal_hook`, and `libc::poll` code in `src/main.rs`, `src/setup.rs`, and `src/lib.rs`. Keep public behavior unchanged at this stage.

- [ ] **Step 6: Verify the foundation on Windows**

Run:

```powershell
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --locked --all-features
```

Expected: platform tests pass; any remaining failures point only to process ownership, credentials, launchd lifecycle, or Unix-only test fixtures that later tasks explicitly address.

- [ ] **Step 7: Commit the platform foundation**

```powershell
git add Cargo.toml Cargo.lock src/lib.rs src/main.rs src/setup.rs src/platform tests/platform.rs
git commit -m "refactor: add cross-platform runtime foundation"
```

---

### Task 2: Add a Cross-Platform Secure Credential Store

**Files:**
- Modify: `Cargo.toml`
- Modify: `src/lib.rs`
- Create: `src/credentials.rs`
- Create: `src/settings.rs`
- Create: `tests/credentials.rs`

**Interfaces:**
- Produces: `CredentialStore::{get,set,delete}` for one named secret
- Produces: `OsCredentialStore::zectrix_api_key() -> Result<OsCredentialStore, CredentialError>`
- Produces: `Settings { device_id: String, page_id: u8 }`; it contains no secret and no privacy toggle because titles are always absent

- [ ] **Step 1: Write failing credential contract tests**

```rust
use codex_zectrix_dashboard::{CredentialStore, MemoryCredentialStore};

#[test]
fn credential_round_trip_never_enters_settings() {
    let store = MemoryCredentialStore::default();
    store.set("secret-value").unwrap();
    assert_eq!(store.get().unwrap().as_deref(), Some("secret-value"));
    store.delete().unwrap();
    assert!(store.get().unwrap().is_none());
}

#[test]
fn credential_errors_do_not_echo_secret_material() {
    let error = codex_zectrix_dashboard::CredentialError::Unavailable;
    assert!(!error.to_string().contains("secret-value"));
}
```

Run: `cargo test --test credentials`

Expected: FAIL because the credential abstraction is absent.

- [ ] **Step 2: Add the native keyring dependency and implementation**

Add:

```toml
keyring = { version = "3", default-features = false, features = ["apple-native", "windows-native"] }
```

Implement the exact contract:

```rust
pub trait CredentialStore: Send + Sync {
    fn get(&self) -> Result<Option<Zeroizing<String>>, CredentialError>;
    fn set(&self, secret: &str) -> Result<(), CredentialError>;
    fn delete(&self) -> Result<(), CredentialError>;
}

pub struct OsCredentialStore { entry: keyring::Entry }
```

Map `keyring::Error::NoEntry` to `Ok(None)` for reads and deletes. Map all other failures to fixed, payload-free `CredentialError` variants. Retain `MemoryCredentialStore` only for tests.

- [ ] **Step 3: Move non-secret settings into a shared module**

```rust
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub device_id: String,
    pub page_id: u8,
}
```

Add `Settings::load`, `Settings::save_atomically`, and `Settings::remove`. Reject page IDs outside `1..=5`. Verify serialized settings never contain the API key or a task/title field.

- [ ] **Step 4: Run credential and serialization tests**

Run:

```powershell
cargo test --test credentials
cargo test --test setup_cli credential -- --nocapture
```

Expected: PASS; no test output contains the sentinel secret.

- [ ] **Step 5: Commit secure storage**

```powershell
git add Cargo.toml Cargo.lock src/credentials.rs src/settings.rs src/lib.rs tests/credentials.rs
git commit -m "feat: use native credential stores on macOS and Windows"
```

---

### Task 3: Own the App-Server Process Tree on Windows

**Files:**
- Modify: `Cargo.toml`
- Modify: `src/app_server.rs`
- Modify: `src/platform/mod.rs`
- Create: `src/platform/process.rs`
- Create: `src/platform/process_unix.rs`
- Create: `src/platform/process_windows.rs`
- Create: `tests/support/fake_codex.rs`
- Modify: `tests/app_server_client.rs`
- Modify: `tests/companion_cli.rs`

**Interfaces:**
- Produces: `OwnedProcess::spawn_codex(program: &Path) -> Result<(Child, OwnedProcess), ProcessError>`
- Produces: closing/dropping `OwnedProcess` terminates the app-server and descendants
- Consumes: `AppServerSession` continues to use child stdin/stdout JSONL

- [ ] **Step 1: Add a cross-platform fake `codex app-server` binary target**

Add a Cargo binary named `codex-zectrix-test-app-server`, gated behind a `test-support` feature, whose `main` reads JSONL and responds to `initialize` and `account/rateLimits/read`. The response must be deterministic:

```rust
json!({
  "id": request["id"],
  "result": {
    "rateLimits": {
      "primary": { "usedPercent": 24, "windowDurationMins": 300, "resetsAt": 1_900_000_000 },
      "secondary": null
    },
    "rateLimitResetCredits": null
  }
})
```

Use `env!("CARGO_BIN_EXE_codex-zectrix-test-app-server")` in integration tests rather than shell scripts, so the same fixtures execute on Windows and macOS.

- [ ] **Step 2: Write the failing Windows ownership test**

```rust
#[test]
fn dropping_the_client_reaps_the_owned_app_server() {
    let sentinel = tempfile::NamedTempFile::new().unwrap();
    let program = env!("CARGO_BIN_EXE_codex-zectrix-test-app-server");
    {
        let client = AppServerClient::new(program);
        assert_eq!(client.read_quota().unwrap().windows[0].used_percent, 24);
    }
    assert!(wait_until_process_record_is_gone(sentinel.path()));
}
```

Run: `cargo test --test app_server_client dropping_the_client_reaps -- --nocapture`

Expected: FAIL or fail to compile because `AppServerSession` still depends on Unix process groups.

- [ ] **Step 3: Move existing Unix process-group behavior unchanged**

Move `/bin/ps`, process groups, `SIGTERM`, and `SIGKILL` into `process_unix.rs` behind `#[cfg(unix)]`. Preserve the current Unix process-tree tests.

- [ ] **Step 4: Implement the Windows Job Object owner**

Add a target-specific `windows-sys` dependency with `Win32_Foundation`, `Win32_System_JobObjects`, and `Win32_System_Threading`. Spawn the child with `CREATE_NO_WINDOW`, create an unnamed Job Object, set `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, and assign the child process handle immediately after spawn. If assignment fails, terminate and wait for the child before returning an error.

The Windows owner must hold the Job handle until drop:

```rust
pub struct OwnedProcess {
    child: Child,
    #[cfg(windows)]
    job: OwnedHandle,
    #[cfg(unix)]
    tree: OwnedProcessTree,
}
```

Drop order is: close stdin, wait up to 3 seconds, request graceful child termination, wait 2 seconds, close/kill the Job, wait for the root child, then join the stdout reader.

- [ ] **Step 5: Route `AppServerSession` through `OwnedProcess`**

Remove direct imports of `std::os::unix::process::CommandExt` and `libc` from `app_server.rs`. Keep reconnect timing and request timeouts unchanged.

- [ ] **Step 6: Verify process cleanup on both platform paths**

Run:

```powershell
cargo test --test app_server_client --all-features -- --nocapture
cargo test --test companion_cli process_tree --all-features -- --nocapture
cargo clippy --all-targets --all-features -- -D warnings
```

Expected: the app-server responds, reconnect tests pass, and no fake app-server or descendant remains after client drop.

- [ ] **Step 7: Commit process ownership**

```powershell
git add Cargo.toml Cargo.lock src/app_server.rs src/platform tests/support/fake_codex.rs tests/app_server_client.rs tests/companion_cli.rs
git commit -m "feat: supervise app-server with Windows Job Objects"
```

---

### Task 4: Read Current Quota and the Latest Top-Level Task Directly

**Files:**
- Modify: `src/app_server.rs`
- Modify: `src/lib.rs`
- Create: `src/codex_status.rs`
- Create: `fixtures/quota-multi-bucket.json`
- Create: `fixtures/thread-list-latest-top-level.json`
- Create: `fixtures/thread-read-running.json`
- Create: `fixtures/thread-read-completed.json`
- Create: `fixtures/thread-read-failed.json`
- Create: `fixtures/thread-read-interrupted.json`
- Create: `tests/codex_status.rs`

**Interfaces:**
- Produces: `QuotaStatus { remaining_percent: u8, resets_at_epoch_seconds: i64 }`
- Produces: `TaskState::{Running, Completed, Failed, Interrupted, Empty, Unavailable}`
- Produces: `CodexStatusSource::{read_quota_status, read_latest_task_state}`
- Consumes: `AppServerClient::rpc_request`

- [ ] **Step 1: Write strict fixture-driven mapping tests**

```rust
#[test]
fn selects_codex_primary_bucket_and_computes_remaining() {
    let quota = parse_quota_status(include_str!("../fixtures/quota-multi-bucket.json")).unwrap();
    assert_eq!(quota.remaining_percent, 76);
    assert_eq!(quota.resets_at_epoch_seconds, 1_900_000_000);
}

#[test]
fn latest_top_level_task_ignores_newer_subagent() {
    let selected = select_latest_top_level(include_str!("../fixtures/thread-list-latest-top-level.json")).unwrap();
    assert_eq!(selected.thread_id, "top-level-2");
}

#[test]
fn turn_statuses_map_without_reading_content() {
    assert_eq!(parse_task_state(include_str!("../fixtures/thread-read-running.json")).unwrap(), TaskState::Running);
    assert_eq!(parse_task_state(include_str!("../fixtures/thread-read-completed.json")).unwrap(), TaskState::Completed);
    assert_eq!(parse_task_state(include_str!("../fixtures/thread-read-failed.json")).unwrap(), TaskState::Failed);
    assert_eq!(parse_task_state(include_str!("../fixtures/thread-read-interrupted.json")).unwrap(), TaskState::Interrupted);
}
```

Run: `cargo test --test codex_status`

Expected: FAIL because the new privacy-safe parsers do not exist.

- [ ] **Step 2: Implement multi-bucket quota selection exactly as specified**

Deserialize only `rateLimitsByLimitId`, legacy `rateLimits`, `primary`, `secondary`, `usedPercent`, and `resetsAt`. Prefer `rateLimitsByLimitId["codex"]`, then legacy `rateLimits`; prefer `primary`, then `secondary`. Reject percentages outside `0..=100` and missing reset timestamps. Compute `100 - usedPercent` with saturation.

- [ ] **Step 3: Implement latest top-level task selection**

Deserialize only:

```rust
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ThreadSummary {
    id: String,
    parent_thread_id: Option<String>,
    updated_at: i64,
    recency_at: Option<i64>,
    status: ThreadStatus,
}
```

Ignore `name`, `preview`, `cwd`, paths, and all content fields. Filter `parentThreadId == null`, then choose the largest `recencyAt.unwrap_or(updatedAt)`.

- [ ] **Step 4: Implement `thread/read` status mapping**

Request `thread/read` with `{ "threadId": selected_id, "includeTurns": true }`. Use `ThreadStatus.active` or latest `TurnStatus.inProgress` as `Running`; otherwise map the latest turn to `Completed`, `Failed`, or `Interrupted`. `ThreadStatus.systemError` maps to `Failed`; an empty list maps to `Empty`; unrecognized shapes return a compatibility error and become `Unavailable` at the runtime boundary.

- [ ] **Step 5: Prove the privacy boundary**

Add a test that supplies sentinels in `name`, `preview`, turn items, prompt text, and project path, then serializes every public status type and rendered diagnostic. Assert none of the sentinel bytes occur.

- [ ] **Step 6: Run direct-source tests and current app-server canary**

Run:

```powershell
cargo test --test codex_status --test app_server_client --all-features
cargo run --locked -- diagnostics
```

Expected: fixture tests pass. The local canary reports supported quota and task methods without printing titles, IDs, or payloads.

- [ ] **Step 7: Commit direct status observation**

```powershell
git add src/app_server.rs src/codex_status.rs src/lib.rs fixtures tests/codex_status.rs tests/app_server_client.rs
git commit -m "feat: read private quota and task status from app-server"
```

### Task 5: Replace the legacy dashboard with the privacy-safe P-style status board

**Files:**
- Create: `src/status_board/mod.rs`
- Create: `src/status_board/model.rs`
- Create: `src/status_board/render.rs`
- Create: `tests/status_board_model.rs`
- Create: `tests/status_board_render.rs`
- Modify: `src/lib.rs`
- Modify: `Cargo.toml`

**Interfaces:**
- Produces: `StatusBoardState`, `BoardTaskState`, `render_status_board`
- Consumes: `QuotaStatus`, `TaskState`
- Output: exactly one 400×300 one-bit PNG suitable for NOTE4

- [ ] **Step 1: Write the public-model privacy tests**

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum BoardTaskState {
    Running,
    Completed,
    Failed,
    Interrupted,
    Empty,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StatusBoardState {
    pub task_state: BoardTaskState,
    pub quota_remaining_percent: Option<u8>,
    pub quota_resets_at_epoch_seconds: Option<i64>,
    pub quota_stale: bool,
    pub task_stale: bool,
    pub uploaded_at_epoch_seconds: Option<i64>,
}
```

The tests must prove that the model has no task ID, title, prompt, path, account identifier, or arbitrary string field, and that conversion from `CodexStatusSource` retains only the six fields above.

Run: `cargo test --test status_board_model`

Expected: FAIL because `status_board` is not implemented.

- [ ] **Step 2: Implement the minimal display model and localized labels**

Map states to fixed copy only:

| State | Large label | Small code |
|---|---|---|
| Running | `运行中` | `RUNNING` |
| Completed | `已完成` | `DONE` |
| Failed | `失败` | `FAILED` |
| Interrupted | `已中断` | `STOPPED` |
| Empty | `无任务` | `EMPTY` |
| Unavailable | `状态未知` | `OFFLINE` |

When `task_stale` is true, add the fixed `数据延迟` warning without altering the last known state. When `quota_stale` is true, show the last known percentage with a small `延迟` marker; when no quota exists, render `--%`.

- [ ] **Step 3: Write renderer contract tests before implementing the design**

Cover all six states plus stale and missing-data variants. Assert:

```rust
let png = render_status_board(&state, &fixed_clock()).unwrap();
let image = image::load_from_memory(&png).unwrap().to_luma8();
assert_eq!(image.dimensions(), (400, 300));
assert!(image.pixels().all(|p| p[0] == 0 || p[0] == 255));
assert!(black_pixel_ratio(&image) > 0.18);
assert!(black_pixel_ratio(&image) < 0.62);
```

Also assert the status panel, quota numeral, reset-time block, and `UPDATED` footer each occupy their assigned non-overlapping bounding box. Run OCR only as an optional development aid; the deterministic pixel and bounds assertions remain the required test oracle.

Run: `cargo test --test status_board_render`

Expected: FAIL because the renderer does not exist.

- [ ] **Step 4: Implement the P-style composition with original geometric elements**

Use only code-drawn rectangles, triangles, circles, strokes, and the bundled u8g2 fonts—no copied Persona artwork, logos, character art, or game assets. The exact 400×300 layout is:

```text
x=0..399, y=0..299
  0..46    top strip: CODEX // NOW, diagonal rule, small index marks
 35..142   jagged black status slab; white Chinese state is the first focal point
124..244   large quota numeral at left; coarse halftone circles behind it
151..242   tilted-looking reset block at right, made from clipped polygons
250..299   UPDATED <local time> footer and stale markers
```

Keep the composition black/white, high-contrast, asymmetric, and readable at arm's length. Render the quota as the largest element. Format reset time in the local timezone as `今天 HH:MM`, `明天 HH:MM`, or `MM-DD HH:MM`; never infer a reset time when the API omits it. `UPDATED` must use `uploaded_at_epoch_seconds`, not the render time.

- [ ] **Step 5: Generate and inspect every preview state**

Add a test helper that writes deterministic previews to `target/p-dashboard-previews/` only when `UPDATE_PREVIEWS=1` is set.

Run:

```powershell
$env:UPDATE_PREVIEWS='1'
cargo test --test status_board_render -- --nocapture
Remove-Item Env:UPDATE_PREVIEWS
```

Inspect `running.png`, `completed.png`, `failed.png`, `interrupted.png`, `empty.png`, `unavailable.png`, and `stale.png` at original resolution. Adjust geometry until no text touches a polygon edge and the information hierarchy remains status → quota → reset → updated time.

- [ ] **Step 6: Run the renderer suite**

Run:

```powershell
cargo test --test status_board_model --test status_board_render
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
```

Expected: all tests and lint checks pass.

- [ ] **Step 7: Commit the new board**

```powershell
git add Cargo.toml Cargo.lock src/status_board src/lib.rs tests/status_board_model.rs tests/status_board_render.rs
git commit -m "feat: render privacy-safe P-style status board"
```

### Task 6: Implement the two-rate scheduler, freshness rules, and reliable changed-frame publishing

**Files:**
- Create: `src/companion.rs`
- Create: `src/freshness.rs`
- Create: `src/bin/codex-zectrix-companion.rs`
- Create: `tests/companion_scheduler.rs`
- Create: `tests/publish_coordinator.rs`
- Modify: `src/publisher.rs`
- Modify: `src/lib.rs`
- Modify: `src/main.rs`
- Modify: `Cargo.toml`

**Interfaces:**
- Produces: `run_companion`, `CompanionScheduler`, `PublishCoordinator`, hidden Windows companion executable
- Consumes: `CodexStatusSource`, `render_status_board`, `ZectrixPublisher`

- [ ] **Step 1: Write deterministic scheduler and freshness tests**

Introduce a fake monotonic clock and fake source. Prove that task status is read immediately and then every 60 seconds, quota is read immediately and then every 600 seconds, and the renderer receives one combined snapshot. Prove these freshness thresholds:

```rust
pub const TASK_STALE_AFTER: Duration = Duration::from_secs(180);
pub const QUOTA_STALE_AFTER: Duration = Duration::from_secs(1_800);

pub struct FreshValue<T> {
    pub value: Option<T>,
    pub last_success_epoch_seconds: Option<i64>,
}
```

A failed refresh preserves the last successful value and becomes visibly stale only after its threshold. A missing initial value renders as unavailable rather than fabricating data.

Run: `cargo test --test companion_scheduler`

Expected: FAIL because the scheduler is absent.

- [ ] **Step 2: Implement the scheduler without reset radar or rollout hooks**

Use one event loop with separate `next_task_poll` and `next_quota_poll` deadlines. Each source call has a bounded timeout. Feed only `StatusBoardState` into rendering; source errors are logged as fixed error categories and never include raw JSON, thread IDs, titles, API keys, paths, or request bodies.

- [ ] **Step 3: Write publisher state-machine tests**

Use a fake PNG renderer, fake ZECTRIX endpoint, and fake clock to prove:

- identical visible state does not upload again;
- a changed task state, quota, reset time, or stale marker uploads one new frame;
- a successful upload commits its completion time as `UPDATED` for the next visible frame;
- a failed upload does not advance `UPDATED`;
- retry delays are 60, 120, 300, then 900 seconds, capped at 900;
- the newest pending visible state supersedes an older pending state;
- hashes cover the rendered frame and never include hidden source data.

Run: `cargo test --test publish_coordinator`

Expected: FAIL until retry and success-time semantics are implemented.

- [ ] **Step 4: Implement `PublishCoordinator`**

Persist only the last successful frame SHA-256, last successful upload epoch, failure count, and next allowed attempt. Render a candidate frame with the previously committed upload time. If its hash differs, upload it; after success, store the server-accepted completion time and render one follow-up frame containing the new `UPDATED` value. Coalesce that follow-up with any newer state and send it on the next allowed scheduler tick, avoiding an immediate two-upload burst.

On failure, retain the newest pending state and use the retry schedule above. Reset the failure count after the next success. Do not retry authentication failures indefinitely: surface `credentials rejected` and wait for explicit setup.

- [ ] **Step 5: Add the shared companion entry point and a windowless Windows binary**

`src/bin/codex-zectrix-companion.rs` begins with:

```rust
#![cfg_attr(windows, windows_subsystem = "windows")]

fn main() -> std::process::ExitCode {
    codex_zectrix_dashboard::companion::run_companion()
}
```

Keep the interactive CLI in `src/main.rs`; both binaries call library code instead of duplicating scheduling logic. The hidden companion writes bounded rotating diagnostics to the app data directory and never opens a console window at login.

- [ ] **Step 6: Verify scheduling, publishing, and shutdown together**

Run:

```powershell
cargo test --test companion_scheduler --test publish_coordinator --test app_server_process
cargo test --all-targets --all-features
```

Expected: fake-clock tests finish without real sleeps, all child processes exit, and every suite passes.

- [ ] **Step 7: Commit runtime behavior**

```powershell
git add Cargo.toml Cargo.lock src/companion.rs src/freshness.rs src/publisher.rs src/main.rs src/bin src/lib.rs tests/companion_scheduler.rs tests/publish_coordinator.rs
git commit -m "feat: schedule and publish changed status frames"
```

### Task 7: Add explicit setup confirmation, Windows Credential Manager, and login autostart

**Files:**
- Create: `src/autostart.rs`
- Create: `tests/setup_flow.rs`
- Create: `tests/windows_autostart.rs`
- Modify: `src/setup.rs`
- Modify: `src/settings.rs`
- Modify: `src/main.rs`
- Modify: `src/lib.rs`

**Interfaces:**
- Produces: `Autostart::{install, uninstall, status}`, `setup`, `preview`, `status`, `uninstall` commands
- Consumes: `CredentialStore`, `ZectrixPublisher`, `render_status_board`

- [ ] **Step 1: Write the cancellation and first-upload confirmation tests**

Drive setup through fake terminal I/O, fake credentials, fake ZECTRIX discovery, and fake autostart. Assert:

- the API key is entered with echo disabled and never appears in output;
- device and page selection precede the preview;
- the preview path and the exact cloud payload summary are shown before confirmation;
- answering anything except the localized affirmative choice cancels without storing the key, settings, uploading, or installing autostart;
- affirmative confirmation performs exactly one first upload and then installs autostart;
- a rejected first upload restores the old settings, removes a newly stored key, and does not install autostart.

Run: `cargo test --test setup_flow`

Expected: FAIL against the current setup flow.

- [ ] **Step 2: Implement the preview-and-confirm setup transaction**

The interactive sequence is fixed:

1. Read the ZECTRIX API key through a non-echo prompt.
2. Discover accessible devices and let the user select NOTE4.
3. Select the target page.
4. Render a sample P-style frame to the app data preview path.
5. State that the cloud receives only the generated PNG, device ID, and page ID; no task title or task content is sent.
6. Ask `确认首次上传到这块屏幕？[y/N]`.
7. Only after `y` or `Y`, store credentials/settings, upload the first frame, and install autostart.

Use a transaction guard to restore prior settings and credential state if upload or autostart installation fails. Zeroize the in-memory API-key buffer after the operation.

- [ ] **Step 3: Write Windows Task Scheduler command tests**

The `windows_autostart` tests inject a fake command runner and assert exact argument arrays, never a shell-joined string. Install uses current-user `schtasks.exe /Create /F /SC ONLOGON /TN "Codex ZECTRIX Dashboard" /TR <absolute companion exe>`. Status uses `/Query`; uninstall uses `/Delete /F`. Quote the task action through the native argument API and reject a relative or missing executable path.

Run: `cargo test --test windows_autostart`

Expected: FAIL because `Autostart` is not implemented.

- [ ] **Step 4: Implement platform autostart backends**

On Windows, use the Task Scheduler command-line client without administrator elevation. Point the task at `codex-zectrix-companion.exe`; its hidden subsystem prevents a login console flash. On macOS, keep the existing launch-agent behavior behind the same trait. Treat `already installed` and `already absent` as idempotent success.

- [ ] **Step 5: Implement operator commands and safe uninstall**

Expose:

```text
codex-zectrix-dashboard setup
codex-zectrix-dashboard preview --output <path>
codex-zectrix-dashboard status
codex-zectrix-dashboard uninstall
```

`status` reports source reachability, last successful task/quota refresh, last successful upload, and autostart state using fixed labels only. `uninstall` removes the scheduled task and local settings/cache; ask separately before deleting the Windows Credential Manager entry. It never removes Codex, firmware, or unrelated scheduled tasks.

- [ ] **Step 6: Run setup and autostart tests**

Run:

```powershell
cargo test --test setup_flow --test windows_autostart --test credentials
cargo test --all-targets --all-features
```

Expected: cancellation is side-effect free, failures roll back, and all tests pass.

- [ ] **Step 7: Commit the Windows setup flow**

```powershell
git add src/autostart.rs src/setup.rs src/settings.rs src/main.rs src/lib.rs tests/setup_flow.rs tests/windows_autostart.rs
git commit -m "feat: confirm first upload and install Windows autostart"
```

### Task 8: Remove legacy data paths and ship a clean Windows package

**Files:**
- Delete: `src/activity_sources.rs`
- Delete: `src/reset_radar.rs`
- Delete: legacy hook-install code from `src/plugin_lifecycle.rs`
- Delete: tests and fixtures that only cover reset radar, task titles, rollout parsing, or hooks
- Create: `.github/workflows/windows-release.yml`
- Create: `scripts/build-release.ps1`
- Create: `scripts/test-clean-install.ps1`
- Create: `docs/agents/windows-install.md`
- Create: `docs/agents/privacy-model.md`
- Create: `docs/agents/p-style-renderer.md`
- Modify: `.codex-plugin/plugin.json`
- Modify: `skills/setup-zectrix-dashboard/SKILL.md`
- Modify: `Cargo.toml`
- Modify: `README.md`
- Modify: `RELEASE_NOTES.md`

**Interfaces:**
- Produces: a Windows x64 ZIP containing both executables and install documentation
- Removes: reset radar, hooks, title/rollout observation, firmware guidance, and Unix-only release assumptions

- [ ] **Step 1: Add package-contract tests before editing release automation**

`scripts/test-clean-install.ps1` must unpack a release ZIP into a fresh temporary directory and verify:

- `codex-zectrix-dashboard.exe` and `codex-zectrix-companion.exe` exist;
- `--version`, `preview`, and `status` run without Rust, Cargo, Node.js, or Python on `PATH`;
- preview output is a 400×300 one-bit PNG;
- setup reaches the confirmation boundary using a local fake endpoint and can cancel without side effects;
- both executables share the same semantic version.

Run against an intentionally incomplete staging directory first.

Expected: FAIL because the Windows package is not assembled.

- [ ] **Step 2: Delete privacy-incompatible legacy paths**

Remove reset radar polling and rendering, rollout/hook parsing, task-title collection, and hook installation from the runtime and plugin instructions. Delete the corresponding modules, fixtures, tests, dependencies, and CLI flags only after the direct app-server and P-style replacements pass. Run `rg -n "reset.radar|activity_sources|hook|rollout|task.title|preview" src tests skills README.md` and inspect every remaining match; retain `preview` only when it refers to the generated board image, never Codex task text.

- [ ] **Step 3: Produce the Windows release archive**

`scripts/build-release.ps1` runs locked MSVC release builds, stages both executables plus `LICENSE`, `README.md`, and `docs/agents/windows-install.md`, creates `codex-zectrix-dashboard-v<version>-windows-x86_64.zip`, and writes a SHA-256 checksum file. It exits nonzero if versions differ, an expected file is missing, or the clean-install test fails.

Run:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/build-release.ps1
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/test-clean-install.ps1 -ArchivePath dist/codex-zectrix-dashboard-v0.2.0-windows-x86_64.zip
```

Expected: the clean install succeeds on a directory containing only packaged files and Windows system tools.

- [ ] **Step 4: Add Windows and macOS CI gates**

The workflow matrix uses `windows-latest` and `macos-latest` and runs:

```text
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
```

The Windows release job additionally runs `scripts/build-release.ps1`, uploads the ZIP and checksum as artifacts, and fails if `test-clean-install.ps1` fails. Keep macOS build coverage so platform abstraction changes do not silently regress the upstream platform.

- [ ] **Step 5: Update plugin metadata and operator documentation**

Bump the crate, CLI, companion, plugin manifest, and release notes to `0.2.0`. The setup skill selects `.exe` paths on Windows and native binaries on macOS. Document:

- Codex desktop/CLI and a ZECTRIX-bound NOTE4 are prerequisites;
- USB is needed only for power/charging or initial vendor setup, not for each status refresh;
- the companion sends only the rendered PNG and ZECTRIX routing identifiers;
- the API key lives in Windows Credential Manager;
- task title/content, reset radar, firmware flashing, and screen control are outside scope;
- troubleshooting commands never ask the user to paste the API key into chat.

- [ ] **Step 6: Run the full release gate and inspect the archive**

Run:

```powershell
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/build-release.ps1
tar -tf dist/codex-zectrix-dashboard-v0.2.0-windows-x86_64.zip
```

Expected: no legacy module is compiled, all checks pass, and the archive contains only the two executables, license, readme, and Windows install guide.

- [ ] **Step 7: Commit packaging and legacy removal**

```powershell
git add -A
git commit -m "release: package Windows P-style dashboard v0.2.0"
```

### Task 9: Complete current-PC canaries and NOTE4 real-device acceptance

**Files:**
- Create after execution: `docs/canary/windows-note4-2026-08-31.md`
- Modify only if defects are found: the smallest source/test files responsible for the failed acceptance step

**Acceptance boundary:**
- The work is not complete until the real NOTE4 changes over Wi-Fi and the evidence document contains actual observed results.
- The operator enters the ZECTRIX key locally; it is never pasted into chat, committed, or copied into the evidence document.
- The first real upload occurs only after the operator sees the preview and explicitly confirms it.

- [ ] **Step 1: Run the local privacy and app-server canaries**

Run:

```powershell
cargo run --locked -- diagnostics
cargo run --locked -- preview --output target/note4-acceptance-preview.png
cargo test --all-targets --all-features
```

Expected: diagnostics reports task/quota method support with fixed labels only; preview is 400×300 one-bit; tests pass. Inspect the preview at original size before contacting the cloud.

- [ ] **Step 2: Start setup and stop at the confirmation boundary**

Run `cargo run --locked -- setup`. The operator enters the API key in the hidden terminal prompt and selects the NOTE4 device/page. Confirm that setup prints the preview path and privacy summary, then waits at `确认首次上传到这块屏幕？[y/N]`. Record that the device/page are correct without recording their raw identifiers.

- [ ] **Step 3: Obtain explicit approval and perform the first upload**

Only after the operator has inspected the preview, accept `y`. Verify that exactly one first-upload request succeeds, credentials are stored in Windows Credential Manager, settings contain only device/page routing IDs, and Task Scheduler contains exactly one `Codex ZECTRIX Dashboard` login task pointing to the packaged companion.

- [ ] **Step 4: Prove USB-independent Wi-Fi updates**

Disconnect the NOTE4 USB data cable or move it to a charge-only supply while keeping its vendor Wi-Fi/cloud connection active. Start a short Codex task and observe within the allowed one-minute task interval that the sign changes to `运行中`. After the task completes, observe `已完成`. Confirm the quota percentage and reset time match the Codex UI within rounding and refresh tolerances.

- [ ] **Step 5: Prove changed-frame suppression and recovery**

Leave task and quota state unchanged across at least two task polls and confirm diagnostics reports no duplicate frame upload. Temporarily block the ZECTRIX endpoint, cause a visible state change, and confirm retries follow 60/120/300-second delays without changing `UPDATED`. Restore connectivity and verify the newest frame uploads, `UPDATED` advances only after success, and no obsolete intermediate frame is sent.

- [ ] **Step 6: Prove login startup and bounded privacy**

Sign out and back in, or reboot if acceptable to the operator. Verify the companion starts without a console window and updates the sign without manually opening the CLI. Search settings, cache, logs, preview metadata, and the acceptance output for a unique sentinel task title used during the test; the sentinel must not appear. Verify the API key is absent from all files and process arguments.

- [ ] **Step 7: Write actual acceptance evidence**

Create `docs/canary/windows-note4-2026-08-31.md` with the tested Windows version, Codex version, dashboard version, packaged checksum, test timestamps, pass/fail for each step above, upload counts, observed refresh delays, and redacted screenshots or photographs if permitted. Record only actual observations; if hardware, credentials, Wi-Fi, or operator confirmation is unavailable, state the exact unmet gate and leave Task 9 incomplete.

- [ ] **Step 8: Re-run checks after any acceptance fix**

For every discovered defect, first add a failing automated regression test, implement the smallest fix, and rerun:

```powershell
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/build-release.ps1
```

Repeat the affected real-device acceptance steps with the rebuilt archive. Do not reuse evidence from a superseded binary.

- [ ] **Step 9: Commit the verified acceptance record**

```powershell
git add docs/canary/windows-note4-2026-08-31.md
git commit -m "test: record Windows NOTE4 acceptance"
```

## Final verification checklist

- [ ] Every field in the approved design appears in `StatusBoardState` or its fixed display copy, and no task title/content field exists.
- [ ] The renderer is exactly 400×300, one-bit, and visually inspected for all states.
- [ ] Task polling is 60 seconds, quota polling is 600 seconds, and unchanged visible frames are not uploaded.
- [ ] `UPDATED` advances only after a successful upload.
- [ ] The API key is stored only in the OS credential backend and never printed.
- [ ] Setup cancellation has no persistent side effects and first upload requires explicit confirmation.
- [ ] Windows login startup uses one current-user scheduled task and no visible console.
- [ ] Reset radar, hooks, titles, rollout content, firmware flashing, and control features are absent.
- [ ] Windows and macOS CI, clean-install packaging, and all automated tests pass.
- [ ] The physical NOTE4 updates over Wi-Fi with USB data disconnected.
- [ ] The real-device evidence file contains actual results from the exact shipped archive.

