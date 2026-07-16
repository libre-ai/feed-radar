# Radar

- **Path:** `apps/radar`
- **Owner:** Experiences / Radar
- **Runtime:** Bun.serve, React 19, PostgreSQL/RLS, bounded workers
- **Tenant model:** personal v1 represented by mandatory tenant ID

## Purpose and actors

Radar lets a person subscribe to chosen feeds, apply visible deterministic rules, inspect why items were retained/rejected and export a portable curated set. The subscriber owns subscriptions/rules; workers fetch untrusted sources; no source becomes trusted content by ingestion alone.

## Journeys

1. **Subscribe safely:** user submits HTTP(S) feed URL, previews bounded discovery, confirms one canonical source and schedule.
2. **Curate:** worker fetches/normalizes an item, evaluates a versioned rule set and records explanation and deduplication identity.
3. **Inspect/replay:** user inspects source, normalized fields, matched rules and decision, then replays against another rule version without mutating history.
4. **Export/delete:** user exports subscriptions/rules/curated decisions with provenance or deletes a source and retained data.

## Non-goals

- web crawler, full-text archive, truth arbiter or recommender profile ;
- executing feed HTML/scripts or following arbitrary discovery links ;
- silently generated opaque ranking ;
- cross-tenant trend analytics ;
- permanent storage of hostile raw response bodies.

## Domain protocol

**Commands:** `PreviewSubscription`, `AddSubscription`, `UpdateSubscription`, `RemoveSubscription`, `CreateRuleSet`, `ActivateRuleSet`, `ScheduleFetch`, `RecordFetchResult`, `ReplayRuleSet`, `ExportCuration`, `DeleteCurationData`.

**Queries:** `ListSubscriptions`, `GetFetchEvidence`, `ListCuratedItems`, `ExplainDecision`, `CompareRuleReplays`, `GetExport`.

**Events:** `SubscriptionAdded`, `FeedFetched`, `FeedFetchRejected`, `ItemNormalized`, `ItemDeduplicated`, `CurationDecided`, `RuleSetActivated`, `RuleSetReplayed`, `CurationExported`, `CurationDataDeleted`.

Fetch jobs are leased and idempotent by tenant/source/scheduled-window. Decision history is append-only; replay creates a new projection linked to original normalized item.

## Refusal matrix

| Code | Refusal |
| --- | --- |
| `radar.url_scheme_forbidden` | source is not approved HTTP(S) |
| `radar.destination_forbidden` | DNS/IP resolves to loopback, private, link-local or metadata range |
| `radar.redirect_forbidden` | redirect exceeds bound or changes to forbidden destination |
| `radar.body_too_large` | compressed/decompressed body exceeds contract |
| `radar.media_type_unsupported` | payload is not an approved feed type |
| `radar.parse_budget_exceeded` | CPU/time/memory/node limit exceeded |
| `radar.rule_invalid` | unknown operator, unbounded expression or missing explanation label |
| `radar.tenant_mismatch` | source/rule/item belongs to another tenant |
| `radar.revision_stale` | subscription/rule mutation uses stale revision |

A rejected fetch records bounded metadata and rule ID, never the full hostile body.

## Data

Radar owns tenant subscriptions, immutable rule versions, schedules, normalized items, decisions and exports. PostgreSQL is authoritative; Redis is lease/cache only. Raw bodies are processed in quarantine and discarded after normalization; failed quarantine and normalized items/decisions follow ADR-0002 section 3 retention. Blobs are permitted only for explicit user exports with expiry. Migration source is OPML/RSS/Atom/JSON Feed supplied by the user plus archived contract fixtures—not historical service tables.

## Authentication and authorization

All personal API routes require opaque browser session and tenant. Internal workers receive attenuated Biscuit tokens limited to one tenant, source ID, `fetch`/`record` operations and short expiry. Authorizer validates destination policy again before network access. RLS applies tenant ID to every authoritative table. Export download tokens are one-use, short-lived and not Biscuit/browser storage.

## Runtime boundaries

Bun owns scheduling, HTTP orchestration, persistence and UI. Hostile parsing and deterministic rule evaluation are Rust candidates: pure feed parser/rule evaluator exposed through WIT/WASM or isolated worker HTTP only after budget tests. Rust receives bytes plus explicit limits and emits canonical JSON; it receives no DB credential.

## Accessibility and degraded mode

Decision explanations are textual and do not rely on color. Keyboard users can add/edit sources and inspect rules. Fetch outage leaves previous items and explicit stale timestamps available. Worker/Redis outage pauses new fetches without duplicate decisions; manual export of retained data remains available.

## Contracts

- Feed Fetch Request/Result v1 — `contracts/schemas/feed-fetch.v1.schema.json` ;
- Rule Set v1 — `contracts/schemas/curation-rule-set.v1.schema.json` ;
- Curated Item Export v1 — `contracts/schemas/curated-item-export.v1.schema.json` ;
- Radar API — `contracts/openapi/radar.v1.yaml` ;
- hostile parser/rules — `contracts/wit/radar-engine-v1/world.wit`.

## Evidence

Unit tests cover canonical URL, idempotency, deduplication and explanations. Contract tests use malicious XML/HTML, compression bombs, redirect chains and unknown fields. Integration tests use local DNS/HTTP fixtures and PostgreSQL RLS. E2E covers subscribe/decision/replay/export/delete. Security gates prove SSRF denial, bounded resources, no raw-body logs and cross-tenant refusal.

## Work packages

1. fetch/rule/export contracts and hostile corpus — Canonical Core ;
2. network quarantine and worker lease model — Web Platform ;
3. bounded parser/rule evaluator — Specialized Rust ;
4. tenant data/API/UI — Experiences ;
5. replay, RLS, SSRF and failure qualification — Infrastructure and Release.

Network and parser packages can proceed in parallel against fixtures; persistence starts from the accepted event and ADR-0002 retention rules.

## Release and rollback

Release requires deterministic replay, duplicate-job proof, SSRF/body/parser budgets, RLS and portable export. Schema migrations use expand/migrate/contract; destructive contraction waits one release. Rollback pauses workers first, restores compatible app/worker artifacts and keeps append-only decisions. It never reprocesses quarantined bytes implicitly.
