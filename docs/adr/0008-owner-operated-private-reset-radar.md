---
status: superseded by ADR-0009
---

# Use an owner-operated private reset radar

The reset radar will run as an owner-operated Cloudflare Worker rather than a public shared service. Each developer deploys an independent Worker and protects reads with an owner-held bearer token stored only in Worker secrets and the local macOS Keychain; the public repository contains setup instructions and secret names but no credentials, so changing to a multi-user hosted service would require a separate identity and key-issuance design.

An hourly Cron Trigger validates the complete upstream status response and stores the latest valid radar snapshot in KV. The authenticated read response combines service freshness, a stable derived radar state, and the complete validated upstream payload; an authenticated bootstrap may fetch once only while KV is empty, after which upstream refreshes remain exclusive to the Cron Trigger.
