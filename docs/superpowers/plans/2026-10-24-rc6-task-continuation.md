# rc.6 task continuation repair

> **For agentic workers:** Use superpowers:executing-plans. The user has authorized implementation, testing, public release and local upgrade.

**Goal:** Read the current Desktop execution after a conversation continues under a new history ID; publish rc.6 and preserve local configuration.

**Architecture:** Pass the enumerated root's rollout path into the metadata-only history adapter. Recognize the observed logical-ID/execution-ID filename mapping with strict validation, use only that execution's lifecycle metadata, and never fall back to a superseded ended turn when the current projection is absent/stale. No conversation contents or titles are read by the adapter.

**Tech Stack:** Rust, rusqlite, existing Windows installer and release scripts.

**Spec:** User request in this chat; existing CONTEXT.md and ADR-0010/0011.

## Constraints and review focus

- Preserve existing normal root and child filtering, 180-second active freshness, 24-hour ended freshness, quota handling, and OS credential entry.
- Reject malformed/mismatched continuation names; never associate another conversation's execution.
- An empty/stale continuation projection must not resurrect the previous interrupted turn.
- Exercise Running, Completed, Failed and Interrupted after continuation, including API-to-adapter integration.
- Test and verify configuration preservation during upgrade; no secrets in logs or packages.
- Existing public rc.5 remains available with a known-issue notice.

## Tasks

- [x] Reproduce alias mismatch in an integration regression against the real SQLite adapter and fake API boundary; observe RED.
- [x] Add bounded metadata alias resolution and fail-closed behavior; pass regression and boundary tests.
- [x] Investigate the existing Windows shortcut CI failure and correct it if reproduced or independently confirmed.
- [x] Bump rc.6, update release notes, run fmt/clippy/core+desktop tests, build and smoke-test ZIP.
- [x] Independent final code review; address substantive findings with regression tests.
- [x] Verify local settings/inkcard hashes and credential continuity, install package using the existing updater, confirm new worker and live uploads.
- [x] Publish source/tag/rc.6 ZIP/checksums, verify public assets and CI, annotate rc.5.

## Execution ledger

- Baseline: clean dedicated publication checkout at 203e291; working branch fix/rc6-task-continuation. Reuse the existing checkout to avoid modifying the older development clone.
- Evidence: logical root's latest history turn was interrupted, but its current rollout filename identifies another execution with inProgress and fresh item timestamps. Existing adapter only queries logical ID.
- Regression evidence: continued execution returned Interrupted instead of Running before the fix; all four lifecycle states now pass. Canonical Windows path shortcut test failed before COM normalization and passes afterwards.
- Final review: one Important finding, no Critical/Minor findings. Fixed DB-open failure bypassing alias guard; missing/newer DB + recent Idle API response regression reproduced Interrupted then passed as Unavailable without calling the superseded private turn.
- Review rulings: aggregation across separate tasks remains unchanged; UNC/overlong shortcut paths are not claimed as tested. Live upgrade, packaging and CI gates are recorded below.
- Local harness: reuse the existing build cache through an ignored target/debug directory junction for the credential API boundary test. Full Windows process-detection tests require the native permission context because the sandbox cannot enumerate owned processes.
- Hosted shortcut diagnosis: ASCII targets passed while Chinese targets failed, for both real executables and text fixtures. Replaced WScript.Shell with native IShellLinkW/IPersistFile, retaining ownership checks and supporting existing STA/MTA callers. Removed all temporary workflow probes and diagnostic logging before release.
- Final native verification: 305 core tests and 314 desktop-feature tests passed, plus fmt, clippy, release build and seven-file ZIP smoke checks. The desktop count includes core coverage; these are not 619 distinct tests.
- Hosted verification: [Windows candidate validation](https://github.com/zwe-zy/codex-zectrix-dashboard/actions/runs/40071856633) passed all stages on e1a4785dcfb57e539d93690ebc0a615bec39abe5. One initial fake-process test returned Unavailable; an unchanged full-job rerun passed, including all four shortcut tests, release build and package checks.
- Local final installation: settings, appearance and inkcards remained byte-identical to the rc.5 backup; credential equality was checked in memory without logging the key or its digest. Exactly one rc.6 worker, matching startup/shortcut, fresh Running state and five successful cloud page uploads were verified. This does not claim physical screen refresh confirmation.
- [rc.6 release](https://github.com/zwe-zy/codex-zectrix-dashboard/releases/tag/v0.3.0-rc.6): the downloaded ZIP matches the tested/installed package, SHA-256 d6d14a8b1c4bedad3c9af886d5232507d6e6117bcba703238b326b44afbfcaa3. rc.5 notes recommend upgrading; its original attachments remain unchanged.
