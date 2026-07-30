// Radar rule evaluator — the TypeScript REFERENCE evaluator for the deterministic
// curation decision (PROFILE.md §8, `contracts/wit/radar-engine-v2`). Like
// policy-core-ref for Policy, this is the byte-exact reference against which the
// deferred Rust/WASM engine is checked; it reuses policy-core-ref's RFC 8785 JCS,
// SHA-256 digest and strict JSON parser rather than reimplementing them.
//
// Input is two canonical JSON byte strings (item, rules); output is a
// `radar-rule-evaluation.v1` verdict or a typed refusal, deny-by-default. The
// evaluator authorizes no tenant and touches no host capability.

import { digest, jcs, parseStrictJson } from "@libre-ai/policy-core-ref";

import { normalizeText } from "./normalize-text";

// PROFILE.md §8 byte ceilings — fixed, MUST NOT be lowered.
const MAX_ITEM_BYTES = 262_144;
const MAX_RULES_BYTES = 524_288;

// The identity material label from §7.
const ITEM_IDENTITY_LABEL = "libre-ai.radar-item.v1";
const ITEM_ID_PREFIX = "urn:libre-ai:radar-item:";
const VALUE_LIMIT = 500;

// The refusals §8/§9 can emit, in precedence order.
export type RefusalCode =
  | "body-too-large"
  | "json-invalid"
  | "json-not-canonical"
  | "item-invalid"
  | "rule-invalid";

export interface RuleResult {
  ruleId: string;
  matched: boolean;
}

export interface RuleEvaluation {
  schemaVersion: "libre-ai.radar-rule-evaluation.v1";
  itemDigest: string;
  ruleSetDigest: string;
  ruleSetId: string;
  ruleSetVersion: number;
  decision: "retain" | "reject";
  reasonCode: "radar.rule_matched" | "radar.default_reject";
  decidingRuleId: string | null;
  explanation: string | null;
  ruleResults: RuleResult[];
}

export type RuleEvaluationResult =
  | { ok: true; value: RuleEvaluation }
  | { ok: false; refusal: RefusalCode };

const IDENTIFIER = /^[a-z][a-z0-9_-]{2,63}$/;
const URN = /^urn:libre-ai:[a-z][a-z0-9-]*:[A-Za-z0-9._~-]+$/;
const SHA256 = /^[a-f0-9]{64}$/;
const SOURCE_ID = /^urn:libre-ai:source:[A-Za-z0-9._~-]+$/;
const UTC_INSTANT = /^\d{4}-\d{2}-\d{2}T([01]\d|2[0-3]):[0-5]\d:[0-5]\dZ$/;

// The exact member set of radar-normalized-item.v1 (additionalProperties: false).
const ITEM_MEMBERS = new Set([
  "authors",
  "deduplicationKey",
  "externalId",
  "id",
  "publishedAt",
  "schemaVersion",
  "sourceHost",
  "sourceId",
  "summary",
  "tags",
  "title",
  "updatedAt",
  "url",
]);
const RULE_SET_MEMBERS = new Set([
  "createdAt",
  "id",
  "rules",
  "schemaVersion",
  "status",
  "tenantId",
  "version",
]);
const RULE_MEMBERS = new Set(["decision", "explanation", "field", "id", "operator", "value"]);

const SCALAR_FIELDS = new Set(["title", "summary", "sourceHost"]);
const ARRAY_FIELDS = new Set(["author", "tags"]);
const STRING_OPERATORS = new Set(["equals", "contains", "prefix"]);
const DATE_OPERATORS = new Set(["before", "after"]);

interface Rule {
  id: string;
  field: string;
  operator: string;
  value: string;
  decision: "retain" | "reject";
  explanation: string;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function sha256Hex(bytes: Uint8Array): string {
  return new Bun.CryptoHasher("sha256").update(bytes).digest("hex");
}

function bytesEqual(a: Uint8Array, b: Uint8Array): boolean {
  if (a.length !== b.length) return false;
  for (let i = 0; i < a.length; i++) if (a[i] !== b[i]) return false;
  return true;
}

// Evaluates a curation rule set against a normalized item, both as canonical JSON
// bytes. Returns a `radar-rule-evaluation.v1` verdict or a typed refusal.
export function evaluateRules(itemBytes: Uint8Array, rulesBytes: Uint8Array): RuleEvaluationResult {
  // §8: item length is checked first, then rules; excess is body-too-large before decoding.
  if (itemBytes.length > MAX_ITEM_BYTES || rulesBytes.length > MAX_RULES_BYTES) {
    return { ok: false, refusal: "body-too-large" };
  }

  // Strict UTF-8 + JSON, no duplicate keys (reuses policy-core-ref's parser).
  let item: Record<string, unknown>;
  let rules: Record<string, unknown>;
  try {
    item = parseStrictJson(itemBytes);
    rules = parseStrictJson(rulesBytes);
  } catch {
    return { ok: false, refusal: "json-invalid" };
  }

  // §8: each parsed value reserialized with RFC 8785 must byte-equal its input.
  if (!bytesEqual(jcs(item), itemBytes) || !bytesEqual(jcs(rules), rulesBytes)) {
    return { ok: false, refusal: "json-not-canonical" };
  }

  if (!isItemValid(item)) {
    return { ok: false, refusal: "item-invalid" };
  }

  const parsedRules = parseRuleSet(rules);
  if (!parsedRules) {
    return { ok: false, refusal: "rule-invalid" };
  }

  const { ruleSetId, ruleSetVersion, ruleList } = parsedRules;
  const ruleResults: RuleResult[] = [];
  let deciding: Rule | null = null;
  for (const rule of ruleList) {
    const matched = matchRule(rule, item);
    ruleResults.push({ ruleId: rule.id, matched });
    if (matched && deciding === null) deciding = rule;
  }

  // §8: itemDigest/ruleSetDigest are SHA-256 of the exact canonical inputs (no label).
  const evaluation: RuleEvaluation = {
    schemaVersion: "libre-ai.radar-rule-evaluation.v1",
    itemDigest: sha256Hex(itemBytes),
    ruleSetDigest: sha256Hex(rulesBytes),
    ruleSetId,
    ruleSetVersion,
    decision: deciding ? deciding.decision : "reject",
    reasonCode: deciding ? "radar.rule_matched" : "radar.default_reject",
    decidingRuleId: deciding ? deciding.id : null,
    explanation: deciding ? deciding.explanation : null,
    ruleResults,
  };
  return { ok: true, value: evaluation };
}

// §6/§7 item invariants the reference evaluator re-checks. Deep feed-level
// normalization is the parser's concern; here we verify the closed shape and the
// content-addressed identity (deduplicationKey / id derivation) that a tampered
// item would violate.
function isItemValid(item: Record<string, unknown>): boolean {
  const keys = Object.keys(item);
  if (keys.length !== ITEM_MEMBERS.size) return false;
  for (const key of keys) if (!ITEM_MEMBERS.has(key)) return false;

  if (item.schemaVersion !== "libre-ai.radar-normalized-item.v1") return false;
  const sourceId = item.sourceId;
  if (typeof sourceId !== "string" || !SOURCE_ID.test(sourceId)) return false;
  const deduplicationKey = item.deduplicationKey;
  const id = item.id;
  if (typeof deduplicationKey !== "string" || !SHA256.test(deduplicationKey)) return false;
  if (typeof id !== "string") return false;

  const externalId = item.externalId;
  const url = item.url;
  if (!(externalId === null || typeof externalId === "string")) return false;
  if (!(url === null || typeof url === "string")) return false;

  // §7 identity material — reuses policy-core-ref digest (label || 0x00 || JCS).
  let identity: { kind: string; sourceId: string; value: unknown };
  if (externalId !== null) {
    identity = { kind: "external-id", sourceId, value: externalId };
  } else if (url !== null) {
    identity = { kind: "url", sourceId, value: url };
  } else {
    identity = {
      kind: "content",
      sourceId,
      value: {
        authors: item.authors,
        publishedAt: item.publishedAt,
        summary: item.summary,
        tags: item.tags,
        title: item.title,
        updatedAt: item.updatedAt,
      },
    };
  }
  const expectedKey = digest(ITEM_IDENTITY_LABEL, identity);
  return deduplicationKey === expectedKey && id === `${ITEM_ID_PREFIX}${expectedKey}`;
}

interface ParsedRuleSet {
  ruleSetId: string;
  ruleSetVersion: number;
  ruleList: Rule[];
}

// §8 rule-set validation: closed shape, version bounds, unique rule ids, valid
// field/operator pairing, and — crucially — every value already in normalized
// form. `status`, `tenantId`, `createdAt` do not affect matching.
function parseRuleSet(rules: Record<string, unknown>): ParsedRuleSet | null {
  const keys = Object.keys(rules);
  if (keys.length !== RULE_SET_MEMBERS.size) return null;
  for (const key of keys) if (!RULE_SET_MEMBERS.has(key)) return null;

  if (rules.schemaVersion !== "libre-ai.curation-rule-set.v2") return null;
  const id = rules.id;
  if (typeof id !== "string" || !URN.test(id)) return null;
  const version = rules.version;
  if (
    typeof version !== "number" ||
    !Number.isInteger(version) ||
    version < 1 ||
    version > 2_147_483_647
  ) {
    return null;
  }
  const rawRules = rules.rules;
  if (!Array.isArray(rawRules) || rawRules.length < 1 || rawRules.length > 100) return null;

  const ruleList: Rule[] = [];
  const seenIds = new Set<string>();
  for (const raw of rawRules) {
    const rule = parseRule(raw);
    if (!rule || seenIds.has(rule.id)) return null;
    seenIds.add(rule.id);
    ruleList.push(rule);
  }
  return { ruleSetId: id, ruleSetVersion: version, ruleList };
}

function parseRule(raw: unknown): Rule | null {
  if (!isRecord(raw)) return null;
  const keys = Object.keys(raw);
  if (keys.length !== RULE_MEMBERS.size) return null;
  for (const key of keys) if (!RULE_MEMBERS.has(key)) return null;

  const { id, field, operator, value, decision, explanation } = raw;
  if (typeof id !== "string" || !IDENTIFIER.test(id)) return null;
  if (typeof field !== "string" || typeof operator !== "string") return null;
  if (typeof value !== "string" || value.length < 1 || value.length > 500) return null;
  if (decision !== "retain" && decision !== "reject") return null;
  if (typeof explanation !== "string" || explanation.length < 1 || explanation.length > 500) {
    return null;
  }
  if (!isFieldOperatorAllowed(field, operator)) return null;

  // §8: a publishedAt value must be an exact valid UTC instant; every other value
  // must already equal normalize-text(value, 500).
  if (field === "publishedAt") {
    if (!UTC_INSTANT.test(value)) return null;
  } else if (value !== normalizeText(value, VALUE_LIMIT)) {
    return null;
  }
  return { id, field, operator, value, decision, explanation };
}

function isFieldOperatorAllowed(field: string, operator: string): boolean {
  if (SCALAR_FIELDS.has(field) || ARRAY_FIELDS.has(field)) return STRING_OPERATORS.has(operator);
  if (field === "publishedAt") return DATE_OPERATORS.has(operator);
  return false;
}

function matchRule(rule: Rule, item: Record<string, unknown>): boolean {
  if (SCALAR_FIELDS.has(rule.field)) {
    const target = item[rule.field];
    return typeof target === "string" && matchScalar(rule.operator, target, rule.value);
  }
  if (rule.field === "author" || rule.field === "tags") {
    // §8: rule field `author` matches against the item's `authors` array.
    const members = rule.field === "author" ? item.authors : item.tags;
    if (!Array.isArray(members)) return false;
    return members.some(
      (member) => typeof member === "string" && matchScalar(rule.operator, member, rule.value),
    );
  }
  // publishedAt
  const instant = item.publishedAt;
  if (typeof instant !== "string") return false;
  return matchDate(rule.operator, instant, rule.value);
}

// §8: equals/contains/prefix over Unicode scalar values, exact and case-sensitive.
function matchScalar(operator: string, target: string, value: string): boolean {
  const t = [...target];
  const v = [...value];
  if (operator === "equals") return target === value;
  if (operator === "prefix") {
    if (v.length > t.length) return false;
    for (let i = 0; i < v.length; i++) if (t[i] !== v[i]) return false;
    return true;
  }
  // contains: a contiguous scalar subsequence.
  if (v.length === 0) return true;
  if (v.length > t.length) return false;
  for (let start = 0; start + v.length <= t.length; start++) {
    let ok = true;
    for (let i = 0; i < v.length; i++) {
      if (t[start + i] !== v[i]) {
        ok = false;
        break;
      }
    }
    if (ok) return true;
  }
  return false;
}

// §8: before/after compare instants strictly; equality is false, null is false.
function matchDate(operator: string, instant: string, value: string): boolean {
  const target = Date.parse(instant);
  const bound = Date.parse(value);
  if (Number.isNaN(target) || Number.isNaN(bound)) return false;
  if (operator === "before") return target < bound;
  return target > bound;
}
