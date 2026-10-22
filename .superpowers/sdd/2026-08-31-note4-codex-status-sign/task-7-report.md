# Task 7 implementation report

## Scope

Implemented the approved Task 7 operator flow only. Legacy modules remain compiled for Task 8. No real setup, cloud request, credential-store mutation, scheduled-task mutation, app-data write, or physical upload was performed while implementing or testing this task.

## Genuine RED / GREEN evidence

1. Windows autostart RED:
   - Command: `note4-cargo-msvc.cmd test --test windows_autostart`
   - Observed failure: the public `Autostart`, command-runner, outcome, status, and error APIs did not exist.
   - GREEN: 4/4 native Windows tests passed after implementing validation, native argument arrays, exit-code mapping, and null-stdio execution.
2. Setup transaction RED:
   - Command: `note4-cargo-msvc.cmd test --test setup_flow`
   - Observed failure: the injected terminal/cloud/file-store/renderer/clock setup API did not exist.
   - GREEN: the initial 6/6 transaction tests passed after implementing cancellation, confirmation, single upload, rollback, and durable follow-up state.
3. Operator/status/runtime RED:
   - Commands: `note4-cargo-msvc.cmd test --test setup_flow` and the focused scheduler test.
   - Observed failure: fixed-label status/uninstall/preview APIs and scheduler success timestamps did not exist.
   - GREEN: offline preview, status, uninstall, and scheduler success-boundary tests passed.
4. Preview CLI RED:
   - Command: focused `preview_command_needs_only_output_and_touches_no_app_state`.
   - Observed failure: `preview --output` still rejected the invocation with `缺少 --input`.
   - GREEN: the command emitted the deterministic Task 5 PNG and created no app-data directory.
5. Snapshot rollback RED:
   - Command: focused `snapshot_failures_after_confirmation_restore_the_preexisting_preview`.
   - Observed failure: a settings-snapshot read error left the newly rendered preview in place.
   - GREEN: all settings/state/credential/autostart snapshot failures restore the prior preview before returning a fixed category.
6. First-upload status RED:
   - Command: focused `status_reports_setup_first_upload_before_the_companion_writes_runtime_status`.
   - Observed failure: status reported the upload time as unavailable before the first companion tick.
   - GREEN: status safely falls back to the four-field publisher state timestamp without rendering its hash.
7. Corrupt runtime-state RED:
   - Command: focused `corrupt_runtime_status_fails_closed_and_preserves_original_bytes`.
   - Observed failure: the companion defaulted the corrupt file and exited with setup-required instead of invalid-configuration.
   - GREEN: corruption now exits closed, preserves the original bytes, and writes only the fixed `invalid_configuration` diagnostic.

Final Task 7 focused counts before the full gate: setup flow 15/15, Windows autostart 4/4, companion scheduler 9/9 on the non-feature focused run.

## Fix round 1 RED / GREEN evidence

1. Upload completion clock RED:
   - A sequence clock represented request start at 1000 and successful cloud completion at 1121, more than 60 seconds later.
   - Observed failure: the persisted upload timestamp was 1000 rather than 1121, so the deferred UPDATED deadline was based on request start.
   - GREEN: coordinator request time is sampled before the call, while `FirstUploadPublisher` samples completion only after `upload_png` returns success. The test also locks the event order `request clock -> upload -> completion clock`, the persisted completion/deadline values, and no follow-up at completion +59.
2. Post-upload rollback RED:
   - Persistent publisher-state restore failure and persistent scheduled-task removal failure both collapsed to the generic `setup rollback unavailable` category.
   - GREEN: each post-upload branch retains `screen may already have changed`; a failed reversible rollback adds the fixed `reversible local rollback may be incomplete` suffix. Both paths still make exactly one upload and expose no private input.
3. Obsolete setup CLI safety RED:
   - `test --test setup_cli` ran zero tests on Windows, while its Unix-only source contained stale setup subprocess cases that could reach a real macOS keyring, launchctl, or user Library directory after the Task 7 refactor.
   - GREEN: the obsolete cases were replaced by one cross-platform offline operator-preview test confined to a temporary output path. It sets unreachable source/cloud sentinels, creates no app-data directory, and is non-zero on Windows without platform-state access.
4. Protocol-valid `notLoaded` RED:
   - A real fake-app-server `thread/read` response with the valid `notLoaded` state was reported as `codex_task_method=incompatible` by both production status mapping and diagnostics.
   - GREEN: all protocol-valid `Ok(TaskState)` values map to `supported`; only `Err(Incompatible)` maps to `incompatible`. The regression exercises the production mapper through the real app-server client and diagnostics command.
5. Semantic state validation RED:
   - Empty, whitespace, traversal-like, non-ASCII, and oversized routing IDs were accepted by Settings load/save; negative and beyond-year-9999 runtime timestamps were accepted; fixed status output treated those valid-JSON values as authoritative.
   - GREEN: routing IDs are bounded to 1-128 safe ASCII routing characters, timestamps are bounded to the supported Unix calendar range, and both load/save/status validate semantics. Invalid status data becomes `not_configured`/`unavailable`; invalid fallback publisher timestamps are ignored.
6. Public Settings Debug RED:
   - Derived Debug rendered the private routing-ID sentinel.
   - GREEN: custom Debug emits `[redacted]` and the page only.
7. Fallible terminal/output RED:
   - `ConsoleTerminal` used `print!`/`println!`, which could panic or hide a closed output boundary, and final success-output failure became generic local-state failure after upload/autostart had completed.
   - GREEN: ConsoleTerminal accepts an injected `Write`, propagates broken-pipe errors for lines and prompts, and production uses fallible writes. A failed final notice reports the fixed completed/screen-may-have-changed category while retaining the successfully committed setup.

## Interactive transaction and privacy boundary

The injected transaction order is:

1. dedicated non-echo secret read into `Zeroizing<String>`;
2. compatible NOTE4 discovery using the in-memory key;
3. safe ordinal device selection, then page 1–5 selection;
4. exact deterministic Task 5 400×300 one-bit preview render and scoped preview write;
5. preview path and the bounded cloud-payload summary;
6. exact prompt `确认首次上传到这块屏幕？[y/N]`;
7. only trimmed `y` or `Y` continues;
8. credential write, settings write, one Task 6 publisher-boundary upload, four-field publisher-state write, then autostart install.

The transcript uses synthetic `NOTE4 N` aliases and never renders routing identifiers. Tests cover secret, routing, PNG, task-content, and arbitrary-error sentinels across successful, cancelled, pre-confirmation failure, confirmed failure, status, and uninstall paths.

Cancellation restores the pending preview byte-for-byte, or removes a newly created preview, and leaves credentials/settings/publisher state/autostart/upload untouched. It prints the fixed cancellation line only after the already required preview/summary/confirmation interaction.

## Rollback matrix

| Failure point | Upload count | Reversible local result | Fixed public category |
| --- | ---: | --- | --- |
| credential write | 0 | old credential/settings/state/preview/task restored | `credential persistence unavailable` |
| settings write | 0 | old credential/settings/state/preview/task restored | `settings persistence unavailable` |
| rejected upload | 1 | old credential/settings/state/preview/task restored | `first upload rejected` |
| unknown upload result | 1 | old credential/settings/state/preview/task restored | `first upload result unknown` |
| publisher-state write | 1 | old credential/settings/state/preview/task restored | `publisher state unavailable; screen may already have changed` |
| autostart failure, including partial creation | 1 | old local snapshots restored; a newly created exact task is uninstalled | `autostart failed; screen may already have changed` |

An already-present exact app task is neither overwritten by setup nor removed by rollback. Rollback never performs a compensating upload.

If rollback itself fails after the irreversible upload, the state-write and autostart branches preserve their screen-change warning and add the fixed rollback-incomplete warning; they never collapse to the generic pre-upload rollback category. Failure to print the final success line likewise reports that setup completed and the screen may already have changed, without undoing or misclassifying the committed result.

## First-upload durable state

Setup routes the approved sample state and its exact PNG through `StatusFrameCoordinator` and an injected `StatusFramePublisher`. The successful state contains only:

- encoded failure/pending phase;
- last successful frame SHA-256;
- last successful upload epoch seconds;
- next allowed attempt epoch seconds.

The state records the source upload as successful and an UPDATED follow-up due 60 seconds later. Reconstruction is deferred before that deadline, sends exactly one `PublishedUpdated` frame on a later normal scheduler tick, then becomes idle. Setup itself performs one upload only.

The upload time is the clock sampled after the cloud boundary returns success, not the request-start time. The follow-up deadline is therefore completion +60 even when the cloud call lasts longer than a scheduler interval.

## Windows autostart contract

Program: `schtasks.exe` with native `OsString` arrays and null stdin/stdout/stderr.

- Install: `[/Create, /F, /SC, ONLOGON, /TN, "Codex ZECTRIX Dashboard", /TR, "<quoted absolute codex-zectrix-companion.exe>"]`
- Status: `[/Query, /TN, "Codex ZECTRIX Dashboard"]`
- Uninstall: `[/Delete, /F, /TN, "Codex ZECTRIX Dashboard"]`

The action is one native argument containing one quoted absolute executable path. Relative, missing, directory, wrong-name, and wrong-extension targets are rejected before command execution. Spaces and shell metacharacters remain data. Exit 0 is present/success, exit 1 is absent/idempotent delete success where applicable, and other/no exit status is unavailable. No elevation or user-controlled arguments are used.

The same `AutostartControl` abstraction retains a launch-agent implementation on macOS using native `launchctl` argument arrays and the existing label.

## Operator commands

- `setup`: injected confirmed transaction with a thin production wrapper.
- `preview --output <path>`: deterministic 400×300 grayscale, one-bit PNG; no source, cloud, credential, autostart, or app-data access.
- `status`: fixed `setup`, `companion_autostart`, task/quota reachability, task refresh, quota refresh, and upload labels/timestamps only; corrupt state fails closed to unavailable values.
- `uninstall`: idempotently removes the exact app task and an explicit list of app-owned files, never a broad directory; a separate exact prompt controls credential deletion.

Runtime status persists only three optional successful timestamps atomically. The companion writes task/quota timestamps only after successful reads and the upload timestamp only from committed publisher state.

## Files

- Created `src/autostart.rs`
- Created `tests/setup_flow.rs`
- Created `tests/windows_autostart.rs`
- Replaced/refactored `src/setup.rs` into library-owned injected flow plus thin production wrappers
- Modified `src/settings.rs`, `src/companion.rs`, `src/main.rs`, `src/lib.rs`
- Extended `tests/companion_scheduler.rs` for successful-refresh and corrupt-runtime boundaries
- Extended `tests/credentials.rs`, `tests/diagnostics_cli.rs`, and `tests/support/fake_codex.rs` for fix-round semantic and protocol regressions
- Replaced obsolete unsafe `tests/setup_cli.rs` subprocess coverage with a cross-platform offline operator-preview test
- No dependency or lockfile change

## Verification and remaining acceptance boundary

The final fresh Task 7 gate completed successfully with the required MSVC wrapper:

- `test --test setup_flow`: 15 passed, 0 failed.
- `test --test windows_autostart`: 4 passed, 0 failed.
- focused setup/autostart/credentials/companion/publisher/diagnostics/setup-CLI suite: 73 passed, 0 failed.
- `test --all-targets --all-features`: 166 passed, 0 failed.
- `fmt --check`: clean.
- `clippy --all-targets --all-features -- -D warnings`: clean.
- `build --locked --all-features`: succeeded.
- PE inspection: dashboard subsystem 3 (console), companion subsystem 2 (Windows GUI).
- `git diff --check`: passed (Git emitted only the repository's existing LF-to-CRLF conversion notices).
- no `codex-zectrix-*` test child process remained.
- no preview/settings/status-frame/runtime-status/companion-log artifact was left in the worktree or the production app-data directory.

All Task 7 tests used injected fakes, temporary directories, or existing local-loopback fixtures. The real production `setup`, `status`, and `uninstall` commands were not invoked, so the verification made zero real cloud, Credential Manager, scheduled-task, app-data, or physical-display mutations.

Task 9 still owns the first real cloud upload, physical NOTE4 result, real Credential Manager entry, real scheduled task, Wi-Fi/USB-independent update, and sign-out/reboot acceptance. Task 8 still owns legacy removal and packaging.
