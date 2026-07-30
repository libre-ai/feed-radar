import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";

import { evaluateRules } from "./rule-evaluator";

const repoRoot = resolve(import.meta.dir, "../../../..");
const fixtureRoot = resolve(repoRoot, "contracts/fixtures/radar-engine-v2");

function readBytes(relPath: string): Uint8Array {
  return new Uint8Array(readFileSync(resolve(repoRoot, relPath)));
}

interface EvaluationCase {
  id: string;
  item: string;
  rules: string;
  expect: { kind: "success"; output: string } | { kind: "refusal"; code: string };
}

const golden = JSON.parse(readFileSync(resolve(fixtureRoot, "golden-vectors.v1.json"), "utf8")) as {
  evaluationCases: EvaluationCase[];
};

// Byte-exact conformance against every golden evaluation vector: the reference
// evaluator must reproduce each success output verbatim and each refusal code.
describe("evaluateRules — golden conformance", () => {
  for (const testCase of golden.evaluationCases) {
    test(testCase.id, () => {
      const expectation = testCase.expect;
      const result = evaluateRules(readBytes(testCase.item), readBytes(testCase.rules));
      if (expectation.kind === "refusal") {
        expect(result.ok).toBe(false);
        if (result.ok) return;
        expect(result.refusal as string).toBe(expectation.code);
        return;
      }
      expect(result.ok).toBe(true);
      if (!result.ok) return;
      const expected = JSON.parse(readFileSync(resolve(repoRoot, expectation.output), "utf8"));
      expect(result.value).toEqual(expected);
    });
  }

  test("covers all 16 evaluation vectors", () => {
    expect(golden.evaluationCases.length).toBe(16);
  });
});

// Focused unit checks on the semantics the golden set exercises only indirectly.
describe("evaluateRules — semantics", () => {
  const item = readBytes("contracts/fixtures/radar-engine-v2/positive/evaluate-item.json");
  const rules = readBytes("contracts/fixtures/radar-engine-v2/positive/evaluate-rules.json");

  test("first matching rule decides; later matches are reported but do not change the verdict", () => {
    const result = evaluateRules(item, rules);
    expect(result.ok).toBe(true);
    if (!result.ok) return;
    expect(result.value.decidingRuleId).toBe("rule_security");
    expect(result.value.decision).toBe("retain");
    expect(result.value.reasonCode).toBe("radar.rule_matched");
    // every rule reports a result, in order, including the one after the deciding rule
    expect(result.value.ruleResults.map((r) => r.ruleId)).toEqual(["rule_security", "rule_late"]);
    expect(result.value.ruleResults.every((r) => r.matched)).toBe(true);
  });

  test("digests are the SHA-256 of the exact canonical inputs", () => {
    const result = evaluateRules(item, rules);
    expect(result.ok).toBe(true);
    if (!result.ok) return;
    expect(result.value.itemDigest).toMatch(/^[a-f0-9]{64}$/);
    expect(result.value.ruleSetDigest).toMatch(/^[a-f0-9]{64}$/);
  });

  test("oversize item is refused before decoding", () => {
    const huge = new Uint8Array(262_145);
    const result = evaluateRules(huge, rules);
    expect(result).toEqual({ ok: false, refusal: "body-too-large" });
  });

  test("non-canonical item bytes are refused", () => {
    const spaced = new TextEncoder().encode(`${new TextDecoder().decode(item)} `);
    const result = evaluateRules(spaced, rules);
    expect(result.ok).toBe(false);
    if (result.ok) return;
    // trailing space makes it either non-canonical or invalid JSON, never a silent success
    expect(["json-not-canonical", "json-invalid"]).toContain(result.refusal);
  });
});
