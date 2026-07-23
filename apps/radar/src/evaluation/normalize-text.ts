// Radar text normalization — PROFILE.md §6.1 (Radar engine v2 normative profile).
// Deterministic, pure, Unicode 15.1. Used by the reference rule evaluator to
// verify that every non-date rule value is already in normalized form (a rule
// carrying a non-normalized value is `rule-invalid`).

// Scalars deleted in step 3: C0 controls (excluding the whitespace already
// mapped in step 2), DEL + C1 controls, bidi overrides/isolates, and BOM.
const DELETED = new Set<number>();
for (let cp = 0x00; cp <= 0x1f; cp++) DELETED.add(cp);
for (let cp = 0x7f; cp <= 0x9f; cp++) DELETED.add(cp);
for (let cp = 0x202a; cp <= 0x202e; cp++) DELETED.add(cp);
for (let cp = 0x2066; cp <= 0x2069; cp++) DELETED.add(cp);
DELETED.add(0xfeff);

// `White_Space`-property scalars mapped to U+0020 in step 2. The set below is the
// Unicode `White_Space=yes` list; every one becomes an ASCII space before the
// control-deletion step, so a tab or a no-break space collapses like a space.
const WHITE_SPACE = new Set<number>([
  0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x20, 0x85, 0xa0, 0x1680, 0x2000, 0x2001, 0x2002, 0x2003, 0x2004,
  0x2005, 0x2006, 0x2007, 0x2008, 0x2009, 0x200a, 0x2028, 0x2029, 0x202f, 0x205f, 0x3000,
]);

// Applies the five ordered steps of `normalize-text(value, limit)` from §6.1.
export function normalizeText(value: string, limit: number): string {
  // 1. NFC.
  const nfc = value.normalize("NFC");
  // 2 + 3: map White_Space to U+0020, delete the listed control/bidi scalars.
  let mapped = "";
  for (const scalar of nfc) {
    const cp = scalar.codePointAt(0) as number;
    if (WHITE_SPACE.has(cp)) {
      mapped += " ";
    } else if (!DELETED.has(cp)) {
      mapped += scalar;
    }
  }
  // 4. Collapse runs of U+0020 and trim leading/trailing U+0020.
  const collapsed = mapped.replace(/ +/g, " ").replace(/^ | $/g, "");
  // 5. Keep at most `limit` scalars, then trim a trailing U+0020.
  const scalars = [...collapsed];
  const limited = scalars.length > limit ? scalars.slice(0, limit).join("") : collapsed;
  return limited.endsWith(" ") ? limited.slice(0, -1) : limited;
}
