# Radar

- **Path:** `apps/radar`
- **Purpose:** explainable monitoring and curation from selected sources.
- **Runtime:** Bun/React, Bun.sql, bounded workers.
- **Owns:** subscriptions, product rules, curation state, exports and scheduling.
- **Rust candidate:** hostile feed parsing and deterministic rule engine via WASM/service boundary.
- **Critical gates:** SSRF allowlist, bounded bodies/redirects, PostgreSQL RLS, rule explanations, replay/deduplication and portable exports.
