// THE CHECK: does what the page SAYS still match what the record HOLDS?
//
// Pure functions over strings — the page's HTML and the values read for
// each claim come in as data, so the whole judgement is unit-testable
// and a broken check is a failing test before it is a wrong green
// (design 59a776c5). Fetching is the caller's job and lives at the edge.

import { CLAIMS, claimById, type Claim } from './claims';

/** What a claim resolved to, or why it could not be read. A claim whose
 *  row could not be read is NOT a pass — silence is the one forbidden
 *  failure mode here, as everywhere in this system. */
export type Resolved =
  | Readonly<{ kind: 'read'; value: string }>
  | Readonly<{ kind: 'unreadable'; why: string }>;

export type Finding =
  | Readonly<{ kind: 'agrees'; id: string; value: string }>
  | Readonly<{ kind: 'disagrees'; id: string; asserts: string; onPage: string; inRecord: string }>
  | Readonly<{ kind: 'unreadable'; id: string; asserts: string; why: string }>
  /** Marked on the page, absent from the registry: nothing knows what
   *  would answer it, so it has never been checked despite looking
   *  checked to anyone reading the source. */
  | Readonly<{ kind: 'unregistered'; id: string }>
  /** In the registry, absent from the page: the copy that carried it is
   *  gone, so the row is now checking nothing. */
  | Readonly<{ kind: 'unused'; id: string }>;

export type Report = Readonly<{
  findings: ReadonlyArray<Finding>;
  /** Marks found on the page. The honest coverage figure: it is a
   *  numerator with no knowable denominator, which is exactly why
   *  `prose_changed` exists. */
  marked: number;
  /** True when the page's prose moved since the fingerprint it is
   *  compared against. THE COVERAGE ANSWER: a marked-claims scheme
   *  cannot see an UNMARKED claim, so a page could grow six new
   *  assertions and still report all-green. Nothing can compute the
   *  true denominator — but "the words changed and the claim set did
   *  not" is decidable, has no false positive on an unchanged page,
   *  and is the prompt to go and mark what was added. */
  proseChanged: boolean;
  /** The fingerprint of the prose as checked, to store for next time. */
  fingerprint: string;
}>;

/** Every `data-claim` id marked in the page, in source order. Deliberately
 *  a regex over the served HTML rather than a DOM parse: this runs
 *  against what the browser was actually sent. */
export function markedClaims(html: string): ReadonlyArray<string> {
  return [...html.matchAll(/data-claim="([^"]+)"/g)].map((m) => m[1] as string);
}

/** The text a visitor reads, reduced to something stable: tags, script,
 *  style and runs of whitespace removed. Two renders of unchanged copy
 *  fingerprint alike; an edit to the words does not. */
export function proseOf(html: string): string {
  return html
    .replace(/<(script|style)\b[^>]*>[\s\S]*?<\/\1>/gi, ' ')
    .replace(/<[^>]+>/g, ' ')
    .replace(/\s+/g, ' ')
    .trim();
}

/** A stable, dependency-free digest of the prose. Not a security hash —
 *  it answers one question, "did the words move", and is stored beside
 *  the claim set. */
export function fingerprintOf(html: string): string {
  const text = proseOf(html);
  let h1 = 0x811c9dc5;
  let h2 = 0x01000193;
  for (let i = 0; i < text.length; i += 1) {
    const c = text.charCodeAt(i);
    h1 = Math.imul(h1 ^ c, 0x01000193) >>> 0;
    h2 = Math.imul(h2 + c + i, 0x85ebca6b) >>> 0;
  }
  return `${h1.toString(16).padStart(8, '0')}${h2.toString(16).padStart(8, '0')}`;
}

/** What the page shows for a claim: the text of the element carrying
 *  the mark, tags stripped. `undefined` when the mark is not there. */
export function shownFor(html: string, id: string): string | undefined {
  const m = elementFor(html, id);
  if (m === null) return undefined;
  return proseOf(m.inner);
}

/** The `href` of the element carrying the mark, when it has one. */
export function hrefFor(html: string, id: string): string | undefined {
  const m = elementFor(html, id);
  if (m === null) return undefined;
  return /href="([^"]*)"/.exec(m.open)?.[1];
}

function elementFor(html: string, id: string): Readonly<{ open: string; inner: string }> | null {
  const re = new RegExp(`<([a-zA-Z][\\w-]*)(\\b[^>]*data-claim="${id}"[^>]*)>([\\s\\S]*?)<\\/\\1>`);
  const m = re.exec(html);
  if (m === null) return null;
  return { open: m[2] as string, inner: m[3] as string };
}

/** WHICH SIDE OF THE ELEMENT CARRIES THE CLAIM — as the claim DECLARES,
 *  never inferred from its source. A link usually asserts where it goes
 *  (the CTA's wording is copy, `/login` is the claim, and comparing the
 *  copy to a route would refuse on every rewording), but a link whose
 *  TEXT is the claim exists too, so guessing would be wrong for one of
 *  them. */
export function pageValueFor(html: string, claim: Claim): string | undefined {
  return claim.reads === 'href' ? hrefFor(html, claim.id) : shownFor(html, claim.id);
}

/** Judge the page against the record. `resolved` is what each claim's
 *  source answered — the caller fetched it; this decides what it means. */
export function checkClaims(
  html: string,
  resolved: ReadonlyMap<string, Resolved>,
  previousFingerprint: string | null,
  registry: ReadonlyArray<Claim> = CLAIMS,
): Report {
  const marks = markedClaims(html);
  const findings: Finding[] = [];

  for (const id of marks) {
    const claim = registry.find((c) => c.id === id) ?? claimById(id);
    if (claim === undefined) {
      findings.push({ kind: 'unregistered', id });
      continue;
    }
    const onPage = pageValueFor(html, claim) ?? '';
    const answer = resolved.get(id);
    if (answer === undefined || answer.kind === 'unreadable') {
      findings.push({
        kind: 'unreadable',
        id,
        asserts: claim.asserts,
        why: answer === undefined ? 'nothing resolved this claim' : answer.why,
      });
      continue;
    }
    if (onPage.trim() === answer.value.trim()) {
      findings.push({ kind: 'agrees', id, value: answer.value });
    } else {
      findings.push({
        kind: 'disagrees',
        id,
        asserts: claim.asserts,
        onPage,
        inRecord: answer.value,
      });
    }
  }

  for (const claim of registry) {
    if (!marks.includes(claim.id)) findings.push({ kind: 'unused', id: claim.id });
  }

  const fingerprint = fingerprintOf(html);
  return {
    findings,
    marked: marks.length,
    proseChanged: previousFingerprint !== null && previousFingerprint !== fingerprint,
    fingerprint,
  };
}

/** Does this report refuse? A disagreement or an unreadable claim does;
 *  prose that moved does NOT — it is a prompt to go and mark, not a
 *  statement that anything is wrong. Keeping those apart is what stops
 *  the check becoming the permanently-red alarm nobody reads. */
export function refuses(report: Report): boolean {
  return report.findings.some((f) => f.kind === 'disagrees' || f.kind === 'unreadable');
}

/** One line per finding, for the packet a run files. A verdict must
 *  name what failed. */
export function reportLines(report: Report): ReadonlyArray<string> {
  const lines = report.findings.map((f) => {
    switch (f.kind) {
      case 'agrees':
        return `ok        ${f.id} — ${f.value}`;
      case 'disagrees':
        return `DISAGREES ${f.id} — the page says "${f.onPage}", the record says "${f.inRecord}" (${f.asserts})`;
      case 'unreadable':
        return `UNREADABLE ${f.id} — ${f.why} (${f.asserts}); no evidence is not a pass`;
      case 'unregistered':
        return `UNREGISTERED ${f.id} — marked on the page, no row says what answers it, so it has never been checked`;
      case 'unused':
        return `unused    ${f.id} — registered, not marked on the page; it is checking nothing`;
    }
  });
  if (report.proseChanged) {
    lines.push(
      `PROSE CHANGED — the words moved and the claim set did not (${report.marked} marked). ` +
        'A marked-claims check cannot see an unmarked claim: re-read the page and mark what was added.',
    );
  }
  return lines;
}
