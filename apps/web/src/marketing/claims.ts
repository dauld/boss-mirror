// WHAT THE WEBSITE CLAIMS, AND WHICH ROW ANSWERS IT (David, 2026-09-20;
// design 59a776c5).
//
// "We should have the protocols for how that works to ensure our
// websites all stay correct and effective." Those are two words and two
// protocols. This file is the CORRECT half.
//
// A marketing page is a set of claims about the company, and this
// company keeps its state in a system of record — so a claim with a row
// behind it can be CHECKED rather than proofread. That is CLAUDE.md's
// Orwell line pointed at our own marketing: communication decays when
// language drifts from reality, and a website is where that drift
// happens first and is noticed last, because nobody re-reads a page
// they wrote six months ago.
//
// MARKED CLAIMS, DECIDED BY DAVID: "Marked claims are fine to start,
// let's go with that." A claim is marked in the page source with
// `data-claim="<id>"` and this registry says which row answers it. The
// limit is accepted deliberately and is worth stating where the code
// is, not only in the packet: AN UNMARKED CLAIM IS INVISIBLE TO THIS
// CHECK. Coverage, not checking, is the risk here — the checker's
// prose fingerprint is the honest answer to it.

/** Where a claim's truth is read from, and WHICH SIDE of the marked
 *  element carries it.
 *
 *  `reads` is declared per claim rather than inferred. A link usually
 *  asserts where it GOES — the CTA's wording is copy, `/login` is the
 *  claim, and comparing the copy to a route would refuse on every
 *  rewording — but a link whose TEXT is the claim exists too, so
 *  guessing from the source kind would be wrong for one of them. One
 *  fewer inference is worth one more field. */
export type ClaimSource =
  /** A route this app actually serves. */
  | Readonly<{ kind: 'route'; path: string }>
  /** A fact declared in the repository: a seed's field, a manifest's
   *  value. `where` is the file and `pointer` the field in it, so the
   *  resolver has no lookup table of its own. */
  | Readonly<{ kind: 'tree'; where: string; pointer: string }>;

export type Claim = Readonly<{
  /** The id in the page's `data-claim` attribute. */
  id: string;
  /** What a reader of the page understands this to be asserting — in
   *  the reader's words, because a disagreement is reported to a person
   *  who has to decide whether the PAGE or the ROW is wrong. */
  asserts: string;
  /** Which side of the marked element holds the claimed value. */
  reads: 'text' | 'href';
  source: ClaimSource;
}>;

/** THE REGISTRY. Adding a claim to a page is adding a mark and a row
 *  here; a mark with no row is reported as unregistered, and a row with
 *  no mark as unused, because both are ways for this file and the page
 *  to drift apart (CLAUDE.md 9a).
 *
 *  THE PAGE'S CLAIMS ARE MEASURED, NOT IMAGINED. Design 59a776c5's
 *  table listed hosting rates, the sponsor roll and packet counts;
 *  none are on the page. It asserts three things. When the rates do
 *  arrive they get a row here with `kind: 'registry'` — added then,
 *  not now, because a source kind nothing uses is code written for a
 *  page that does not exist.
 *
 *  THE GITHUB LINK IS A CLAIM AS OF 2026-09-20 (backlog f8af6040).
 *  It could not be one before: checking a link needs something that
 *  declares what right IS, and the mirror URL was a literal in four
 *  shell scripts owned by none of them — one inside a refusal message.
 *  Pointing the claim at one of those scripts would have made a shell
 *  script the authority for a marketing claim and hidden the defect
 *  inside a passing check. So the URL was collapsed into the estate's
 *  one-place file first, and the row below points at THAT — the same
 *  value the publish path reads. */
export const CLAIMS: ReadonlyArray<Claim> = [
  {
    id: 'tenant.name',
    asserts: 'the demo tenant the live view shows is named this',
    reads: 'text',
    // NOT the instance's own `/api/tenant/manifest` — that answers
    // "Algedonic, LLC", the company running BOSS, which is a different
    // thing from the demo tenant the page is showing a window into.
    // Pointing this claim there would have compared two unrelated
    // names and refused forever; the gate caught it.
    source: { kind: 'tree', where: 'examples/brewery/seeds/tenant.toml', pointer: 'meta.display_name' },
  },
  {
    id: 'source.repo',
    asserts: 'the public source of this system lives at this address',
    // The link asserts where it GOES; its text is copy ("Source on
    // GitHub") and rewording it must not refuse.
    reads: 'href',
    // The estate's one-place file, beside the system of record's
    // address and the forge's — read by the publish path through
    // infra/lib/sor.sh (BOSS_MIRROR_URL), so the website and the thing
    // that feeds it cannot disagree about where the mirror is.
    source: { kind: 'tree', where: 'infra/estate/estate.toml', pointer: 'mirror_url' },
  },
  {
    id: 'cta.signin',
    asserts: 'this route exists and serves a sign-in',
    reads: 'href',
    source: { kind: 'route', path: '/login' },
  },
];

export const claimIds: ReadonlyArray<string> = CLAIMS.map((c) => c.id);

export function claimById(id: string): Claim | undefined {
  return CLAIMS.find((c) => c.id === id);
}
