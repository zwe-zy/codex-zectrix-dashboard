# Codex Dashboard for ZECTRIX 0.1.9

Adds the independently sourced reset radar to the 400×300 NOTE4 dashboard. The compact radar distinguishes an active forecast, a recent regular reset announcement, a recent reset-credit announcement, a fresh no-Watch observation, and unavailable evidence without presenting the third-party signal as official account quota.

The Companion requests the public reset-radar status directly at startup and once per hour. Conditional requests, bounded timeouts, a 64 KiB response limit, capability-specific validation, semantic caching, and privacy-safe diagnostics keep this optional source isolated from official quota and task activity.

Task activity now uses a 30-second deadline and official quota a 10-minute deadline. The persistent Companion and its owned app-server session sleep until the nearest source deadline, coalesce overdue observations after wake, avoid heartbeat cache writes, and continue to publish only when visible state changes.

The final 400×300 layout was refined against a physical NOTE4. Official quota now uses a tighter heading-to-number relationship while preserving clear separation between the progress bar, quota guidance, and reset radar. The radar keeps its full-width ten-cell ruler, and the three task rows use a compact, even vertical rhythm with a separately reserved footer.

MVP limitations:

- the reset radar is a third-party probabilistic signal, not an OpenAI commitment or an account-specific reset time;
- no exact Desktop unread blue dot synchronization;
- no 待你 state;
- no authoritative 检查 state;
- no plan progress;
- no task mutation or control;
- no support for other operating systems;
- no support for other display models.

Automated fixture, process, and package validation does not constitute installation or physical NOTE4 acceptance. The 0.1.9 layout and changed-frame path were exercised on the development NOTE4, but each installation must still verify its installed companion, source diagnostics, delivery, and physical readability independently.
