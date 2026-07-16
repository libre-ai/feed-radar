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
| `radar.invalid_limits` | parser input/output byte, item or depth limits are outside v2 ranges |
| `radar.invalid_source` | source ID or final base URL is invalid/non-canonical |
| `radar.body_too_large` | decompressed input exceeds the explicit contract bound |
| `radar.output_too_large` | complete canonical output exceeds the explicit contract bound |
| `radar.media_type_unsupported` | media type or media parameter is not approved |
| `radar.encoding_unsupported` | bytes are not an accepted uncompressed UTF-8 document |
| `radar.feed_malformed` | selected feed syntax is malformed or has duplicate JSON keys |
| `radar.feed_kind_unsupported` | root/version does not match RSS 2.0, Atom 1.0 or JSON Feed 1/1.1 |
| `radar.xml_dtd_forbidden` | XML contains a DTD/entity declaration |
| `radar.xml_entity_forbidden` | XML references a non-predefined named entity |
| `radar.max_depth_exceeded` | complete source tree exceeds the declared depth |
| `radar.max_items_exceeded` | source candidates exceed the declared count before deduplication |
| `radar.json_invalid` | rule-evaluation JSON is malformed, invalid UTF-8 or has duplicate keys |
| `radar.json_not_canonical` | rule-evaluation input is not exact RFC 8785 JSON |
| `radar.item_invalid` | canonical item does not satisfy its strict schema |
| `radar.rule_invalid` | rule set violates schema or deterministic semantic invariants |
| `radar.tenant_mismatch` | source/rule/item belongs to another tenant |
| `radar.revision_stale` | subscription/rule mutation uses stale revision |

The v2 WIT refusal is a closed enum; the host performs the exact `x-y` → `radar.x_y` mapping. A
rejected fetch records bounded metadata only, never hostile body bytes, excerpts, parser diagnostics
or tenant data in its error.

## Data

Radar owns tenant subscriptions, immutable rule versions, schedules, normalized items, decisions and exports. PostgreSQL is authoritative; Redis is lease/cache only. Raw bodies are processed in quarantine and discarded after normalization; failed quarantine and normalized items/decisions follow ADR-0002 section 3 retention. Blobs are permitted only for explicit user exports with expiry. Migration source is OPML/RSS/Atom/JSON Feed supplied by the user plus archived contract fixtures—not historical service tables.

## Authentication and authorization

All personal API routes require opaque browser session and tenant. Internal workers receive attenuated Biscuit tokens limited to one tenant, source ID, `fetch`/`record` operations and short expiry. Authorizer validates destination policy again before network access. RLS applies tenant ID to every authoritative table. Export download tokens are one-use, short-lived and not Biscuit/browser storage.

## Runtime boundaries

Bun owns scheduling, HTTP orchestration, decompression, persistence, authorization and UI. The
candidate WIT/WASM boundary reserves hostile feed parsing and deterministic rule evaluation for a
future specialized Rust engine; this contract step implements no engine. The pure component receives
bytes, authorized source ID, final base URL and explicit limits; it emits canonical JSON, imports no
capability and receives no tenant value, credential, source fetcher or database handle. Browser
command schemas omit server-owned tenant, identity, revision, status and time fields; Bun derives them
from the authenticated session and authoritative state. Public refusals use closed codes and the static
content-free message `Request refused`.

## Accessibility and degraded mode

Decision explanations are textual and do not rely on color. Keyboard users can add/edit sources and inspect rules. Fetch outage leaves previous items and explicit stale timestamps available. Worker/Redis outage pauses new fetches without duplicate decisions; manual export of retained data remains available.

## Contracts

- Feed Fetch Request/Result v1 — `contracts/schemas/feed-fetch.v1.schema.json` ;
- Rule Set v2 — `contracts/schemas/curation-rule-set.v2.schema.json` ;
- Normalized Item/Feed v1 — `contracts/schemas/radar-normalized-item.v1.schema.json` and
  `contracts/schemas/radar-normalized-feed.v1.schema.json` ;
- Rule Evaluation v1 — `contracts/schemas/radar-rule-evaluation.v1.schema.json` ;
- Curated Item Export v2 — `contracts/schemas/curated-item-export.v2.schema.json` ;
- Radar API v2 — `contracts/openapi/radar.v2.yaml` ;
- hostile parser/rules v2 — `contracts/wit/radar-engine-v2/world.wit` and its normative `PROFILE.md` ;
- portable hostile/golden corpus —
  `contracts/fixtures/radar-engine-v2/golden-vectors.v1.json` and
  `contracts/fixtures/radar-engine-v2/security-vectors.v1.json`.

## Evidence

Unit tests cover canonical URL, idempotency, deduplication and explanations. The candidate contract
harness covers 43 parse cases, 16 evaluation cases and 18 generated exact/over boundaries, including
all accepted media/dialects, every closed refusal, DTD/entity/reference XML, HTML, BOM/invalid UTF-8,
duplicate JSON keys, deep unknown fields, UTC rollover, pre-dedup item limits and refusal precedence.
Integration tests use local DNS/HTTP fixtures and PostgreSQL RLS. E2E covers
subscribe/decision/replay/export/delete. Security gates prove SSRF denial, bounded resources,
no raw-body logs and cross-tenant refusal.

## Work packages

1. fetch/rule/export contracts and hostile corpus — Canonical Core ;
2. network quarantine and worker lease model — Web Platform ;
3. bounded parser/rule evaluator — Specialized Rust ;
4. tenant data/API/UI — Experiences ;
5. replay, RLS, SSRF and failure qualification — Infrastructure and Release.

Network quarantine may proceed against fixtures, but parser implementation waits for the independent Architecture and Security agent verdicts. Persistence starts from the accepted event and ADR-0002 retention rules.

## Release and rollback

Release requires deterministic replay, duplicate-job proof, SSRF/body/parser budgets, RLS and portable export. Schema migrations use expand/migrate/contract; destructive contraction waits one release. Rollback pauses workers first, restores compatible app/worker artifacts and keeps append-only decisions. It never reprocesses quarantined bytes implicitly.
