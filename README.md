**English** · [Français](README.fr.md)

> [!NOTE]
> **Reserved · future home of Radar** — rebuilt in the canonical base repository [`libre-ai/libre-ai`](https://github.com/libre-ai/libre-ai) ([multi-repo topology, ADR-0008](https://github.com/libre-ai/libre-ai/blob/main/docs/adr/0008-multi-repo-target-topology-and-brand.md)).
> This repository will reopen as the real product repository when the owner activates it, consuming the base as a versioned dependency. The foundations described below are **being built now** — with links to the code that already exists.

# Radar

**Explainable feed selection and portable curation.** Subscribe to feeds (RSS, Atom, JSON Feed), apply visible deterministic rules to decide which items to keep, inspect why each decision was made rule-by-rule, and export a curated set you control. Workers fetch untrusted sources; no feed becomes trusted just by being ingested.

The canonical brief it answers: _"Help me read only what I chose, explain why"_ — surfacing **every** rule that matched or failed, with **zero** opaque ranking or algorithmic surprise. Built for local-first, for OPML/RSS/Atom/JSON Feed portability, and for replay: compare how the same items would have been decided under a different rule version without rewriting history.

## Why it's different

- **Explainable decisions, not scores.** Every item kept or rejected is traced to the specific rules that matched or failed — you see the reasoning, rule by rule, every time.
- **Portable curation.** Rules, subscriptions, and curated decisions export with provenance as standard OPML/JSON; you own the data you wrote.
- **Replay without rewrite.** Compare how another rule version would have decided the same historical items without mutating the original decisions. History is append-only.
- **Hostile-source safe.** Feed parsing is confined to a capability-free, deterministic engine; items are normalized and deduplicated before they reach the UI. Feeds stay untrusted until _you_ decide they matter.
- **Fail-closed on bad sources.** Pre-network destination policy (SSRF gate) refuses loopback, private ranges, metadata endpoints, and DNS-rebinding tricks before a single socket opens.

## Status — spec-published, foundations under construction

Radar is being rebuilt from locked contracts. It is **not released yet**; the deterministic feed parser and rule evaluator come next, and the contract layer is already proven:

| Foundation                                                         | State        | Evidence                                                                                                                                                            |
| ------------------------------------------------------------------ | ------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Schema contracts v1/v2** — locked feed, rule, and export formats | ✅ built     | 8 JSON schemas + WIT world, 275-line normative PROFILE.md ([#40](https://github.com/libre-ai/libre-ai/pull/40)–[#46](https://github.com/libre-ai/libre-ai/pull/46)) |
| **Golden-vector corpus** — 43 parse cases, security boundaries     | ✅ published | `contracts/fixtures/radar-engine-v2/{golden-vectors.v1.json, security-vectors.v1.json, adversarial/, positive/}`                                                    |
| **Pre-network SSRF gate** — fail-closed destination policy         | ✅ built     | `src/security/destination-policy.ts` + tests, IPv4/IPv6 classification, DNS-rebinding defeat ([#161](https://github.com/libre-ai/libre-ai/pull/161))                |
| **OpenAPI v2** — server and worker surface                         | ✅ specified | `contracts/openapi/radar.v2.yaml` + domain protocol in product brief                                                                                                |
| Hostile feed parser — Rust/WASM component                          | ⏳ next      | WIT interface locked, container-ready, waiting for implementation                                                                                                   |
| Deterministic rule evaluator — policy decision logic               | ⏳ next      | Specification in PROFILE.md; golden vectors target exact semantics                                                                                                  |
| Network quarantine / worker lease model — bounded fetch            | ⏳ next      | Architecture sketched; idempotency, deduplication and credential isolation TBD                                                                                      |
| Tenant API + UI cockpit — subscriptions, rules, decisions, export  | ⏳ wave 3–4  | Deferred: awaits parser + rule engine foundation                                                                                                                    |

This repository is a public reserved home; the legacy implementation it still carries is frozen for reference, and the rebuild happens in the base repository until activation (wave 4). **Benchmark target:** Inoreader ([inoreader.com](https://www.inoreader.com)) — reached through deterministic, auditable curation rather than algorithmic discovery.

## How it works

1. **Subscribe** — a user submits an HTTP(S) feed URL; the pre-network gate classifies the destination before any fetch; a preview scans the head of the feed.
2. **Fetch & normalize** — workers fetch feeds in a quarantined environment and parse them into a canonical normalized schema (Atom, RSS, or JSON Feed all become the same contract).
3. **Evaluate rules** — a deterministic, capability-free engine applies a versioned rule set to each item and records the decision and matched rules.
4. **Inspect & replay** — users see the source, normalized fields, applied rules, and the decided outcome. They can replay against another rule version to compare decisions without mutating history.
5. **Export & delete** — curated items, rules, and subscriptions export as JSON with provenance; users can delete sources and their retained data on demand.

## Architecture — built from interoperable bricks

Radar is a product assembled from independently versioned bricks; each is usable and testable on its own, and the product is their composition (the multi-repo target of [ADR-0008](https://github.com/libre-ai/libre-ai/blob/main/docs/adr/0008-multi-repo-target-topology-and-brand.md)).

| Brick                                           | Role                                        | Interface it exposes / consumes                                                                                                                                               |
| ----------------------------------------------- | ------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **`radar-engine`** (Rust → WASM component)      | The deterministic feed parser and evaluator | WIT world `radar-engine-v2`: `parse(feed-bytes, limits) → items`, `evaluate(items, rule-set) → decisions`                                                                     |
| **`@libre-ai/destination-policy`** (TypeScript) | Pre-network fail-closed gate                | `isForbiddenDestination(ip)`, `evaluateFetchDestination(url)`, `checkRedirect(...)` → refusal enum or ✓                                                                       |
| **`@libre-ai/web-platform`**                    | SSR / Bun BFF foundation                    | Request handler, authenticated session, tenant context                                                                                                                        |
| **Contracts**                                   | Locked interoperability surface             | `feed-fetch.v1`, `curation-rule-set.v2`, `radar-normalized-{feed,item}.v1`, `radar-rule-evaluation.v1`, `curated-item-export.v2`, `radar.v2.yaml`, WIT world + golden vectors |

The worker receives an attenuated authorization token scoped to one tenant and feed source; the engine holds no token, opens no network, and receives no raw response bytes — it works only on normalized inputs under declared byte/item/depth bounds.

## Where the work happens

All active development is in the base repository, under:

- `apps/radar` — the product host (API, worker orchestration, UI cockpit)
- `contracts/schemas/` — feed-fetch, curation rules, normalized items and exports (v1/v2)
- `contracts/wit/radar-engine-v2/` — the WIT world definition and normative PROFILE.md
- `contracts/fixtures/radar-engine-v2/` — golden vectors and security corpus
- `contracts/openapi/radar.v2.yaml` — API surface and worker endpoints
- [`docs/apps/radar.md`](https://github.com/libre-ai/libre-ai/blob/main/docs/apps/radar.md) — the full product brief

To follow progress or contribute, open issues and pull requests in [`libre-ai/libre-ai`](https://github.com/libre-ai/libre-ai). This repository stays reserved until activation.

## License

Multi-licence, declared per path with [REUSE](https://reuse.software/) in [`REUSE.toml`](REUSE.toml):

- `EUPL-1.2` — the Rust application workspace, the UI surface, the PostgreSQL migrations, the database security manifest, the scripts, the end-to-end suite and the workflows under `.github/`;
- `Apache-2.0` — the interoperability schemas under [`schemas/`](schemas) and the sample corpus under [`examples/`](examples);
- `CC-BY-4.0` — editorial documentation and first-party project records.

[`LICENSE`](LICENSE) is the human-readable summary; the canonical policy — scope, precedence, historical grants, data and contribution rules — is the [organization licensing policy](https://github.com/libre-ai/libre-ai/blob/main/LICENSING.md). Revisions previously published under MIT remain available under those terms; no previous licence grant is withdrawn.
