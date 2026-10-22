# Distribute a standalone Windows package

For Windows, supplement ADR-0001 with a standalone x64 ZIP containing a console setup/operator executable and its background companion. End users do not need Rust or Visual Studio. Installation is per-user under `%LOCALAPPDATA%\CodexZectrixDashboard\app\<version>`; configuration remains in the existing data directory and the ZECTRIX key remains in the Windows credential store under the existing service/account names (ADR-0002).

Use HKCU Run for logon startup without administrator rights. Serialize configuration, installation and uninstall; keep a single companion per user/data directory. Preserve the previous program and non-secret state for recovery. A manually downloaded, verified package is the only update source in this release. Legacy scheduled-task migration must verify ownership and stop the old worker before enabling its replacement.

Keep the Rust core, MIT license, original attribution and existing macOS implementation. This decision makes no Linux/macOS release-readiness claim. Unsupported Codex observation capabilities continue to degrade independently (ADR-0006); unknown activity must never be presented as completed work. The NOTE4 status board uploads no task titles or conversation content.
