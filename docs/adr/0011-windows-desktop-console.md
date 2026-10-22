# Add a native Windows desktop console

Supplement ADR-0010 with an eframe/egui console, shipped as `NOTE4控制台.exe`.
Four pages cover overview, inkcards/themes, device setup, and settings/diagnostics.
The companion remains the only persistent observer and uploader. The GUI reads a
bounded local snapshot every two seconds; it does not create an observation session
when navigating or refreshing. An explicit first-run login check may use a short-lived
Codex client. Closing the GUI leaves the companion running.

Reuse the CLI's guarded setup transaction, installation and background controls.
One worker serializes actions. Draft cancellation invalidates late network results;
commit/install/uninstall wait for completion before closing. Secrets remain in memory
only until committed to the existing OS credential entry, and never enter snapshots,
logs, command-line arguments, or debug representations.

Publish schema-2 packages with six hashed files and package.json: GUI, CLI,
companion, LICENSE, THIRD-PARTY-NOTICES.txt and 快速入门.txt. Continue reading exact
schema-1 packages. Do not change the legacy recovery checkpoint or snapshot list.
Manage only verified current-user Start Menu shortcuts; preserve a user-modified
link. Shortcut creation failure warns after installation without undoing a healthy
installation. GUI uninstall delegates delayed cleanup to the verified CLI executable.

Show source freshness independently for primary quota, secondary quota and task status.
Refresh failures revoke live claims and cached values continue aging. Keep password
editing focused during local refresh and discard widget undo history every frame.
Check the GUI's expected installation version under the shared transaction lock;
expose a retry path for pending uninstall journals.
A cached PNG is shown
only if its size, dimensions and hash match the successful upload record. Example
layouts are explicitly labelled and never uploaded by preview selection. A cloud
upload acknowledgement is not evidence of a physical NOTE4 refresh.

The card editor supports a single page/theme and an ordered list of 2–5 independently
selected themes. Dragging or using move buttons edits a local draft; Save applies the
listed destination pages explicitly. Multi-card position maps to page 1–N. Store the
whole layout in one atomic inkcards.json preference; retain legacy single/all decoding
when pages is absent. The shared coordinator applies per-page cooldowns and retries
only to enabled destinations, including a custom single page. Credentials and device
identity are unchanged. A preference save stops and resumes a previously running worker.

Bundle the OFL-licensed Noto Sans SC font, use regular weight for Chinese, and
retain source revision/hash and license. Native Windows validation remains separate
from headless state/layout tests. This release makes no Linux/macOS readiness claim.
