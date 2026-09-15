// The packet-card vocabulary — the data shape and protocol coloring
// behind PacketCard.svelte, the one visual for a job packet anywhere
// a queue renders (feedback d69033dd). Moved here from
// apps/web/src/it/yard/yard.ts when the card was promoted out of the
// yard; yard.ts re-exports these so the definitions live exactly once.

// What a card shows. `branch` is the mono provenance line — a git
// branch in the train yard, the actionable step title in My Day; each
// lens maps its rows into this shape, never the other way around.
export type PacketCardData = Readonly<{
  id: string;
  kind: string;
  branch: string;
  title: string;
  tags: readonly string[];
  sim: boolean;
  skipReason?: string | null;
  /** How many red trains have released this car — the conductor's
   *  `red_trains` stamp. Absent or 0 is a clean car and the card says
   *  nothing; above 0 it wears {@link redTrainsPhrase} as a warn badge.
   *  Optional so a lens that has no notion of a strike (a watchlist row,
   *  a station queue) passes nothing and draws the card it always did. */
  redTrains?: number;
}>;

/** The strike sentence — `1 red train behind it`, `2 red trains behind
 *  it` — and `''` for a clean car, so a caller can append it blindly.
 *  Lives with the card so the yard floor's dock wagon and the My Day
 *  card say the same words for the same count (d6e53a35, 2026-09-14:
 *  a builder's own struck car read clean in their personal queue while
 *  the yard drew it struck). "Behind it" because the reds are consists
 *  the car rode, not verdicts on it — which car turned a consist red is
 *  exactly what nobody knows yet (2bb0d014). */
export function redTrainsPhrase(n: number | undefined): string {
  return (n ?? 0) <= 0 ? '' : `${n} red train${n === 1 ? '' : 's'} behind it`;
}

// The packet partition — whose packet it is (boss-core
// `partition::Partition`, packet 508cc38c): `real` is the operating
// company, `simulated` the demo tenant's synthetic load, `shadow` the
// experiment lane's dry run. One fact, fixed at admission. Mirrors the
// Rust enum's lowercase serde spelling; a word added there without a
// member here parses through `partitionOf` as the derived bool instead.
export const PARTITIONS = ['real', 'simulated', 'shadow'] as const;
export type Partition = (typeof PARTITIONS)[number];

// The wire carries two spellings per packet — `partition` (the word)
// and the legacy `simulated` bool, DERIVED as not-real — and this is
// the one reader for both: the word when present and known, else the
// bool (an N-1 server), else real (a pre-flag row). Called at the fetch
// boundary; every lens then reads the type, never the bool.
export function partitionOf(
  r: Readonly<{ partition?: unknown; simulated?: unknown }>,
): Partition {
  const word = r.partition;
  if (typeof word === 'string' && (PARTITIONS as readonly string[]).includes(word)) {
    return word as Partition;
  }
  return r.simulated === true ? 'simulated' : 'real';
}

// The facts a packet carries about being simulated. Every field is
// optional: a lens passes whatever its rows hold (My Day's assignment
// rows have no job metadata, only the flag and the tags).
export type SimFacts = Readonly<{
  partition?: Partition;
  simulated?: boolean;
  tags?: readonly string[];
  metadata?: Record<string, unknown> | null;
}>;

// Simulated is a fact on the packet, never an inference from where it
// came from. The Job's own admission-fixed partition is the source of
// truth — and this answers "is this NOT real?", so a shadow packet reads
// as not-real here exactly as a simulated one does (rendering it as its
// own thing is car 4 of 508cc38c); the legacy `simulated` bool, then
// the tag / metadata conventions, stay as fallback for packets that
// predate the field — a `real` word does not silence them, because a
// packet from before the column reads `real` there and carries the tag
// instead (there was no backfill). Lives here with the card so every
// queue lens answers "is this real?" identically (CLAUDE.md §9a) —
// yard.ts re-exports it, and My Day calls it on its assignment rows.
export function isSim(j: SimFacts): boolean {
  if (j.partition !== undefined && j.partition !== 'real') return true;
  if (j.simulated === true) return true;
  const tagged = (j.tags ?? []).some(t =>
    ['sim', 'simulated', 'synthetic'].includes(t.toLowerCase()),
  );
  return tagged || (j.metadata as { simulated?: boolean } | null)?.simulated === true;
}

// Categorical hues for protocol chips, tuned to sit quietly on the
// VOID/INK grounds. SIGNAL teal is deliberately absent — it stays the
// one live accent — and ok/warn/err stay reserved for state.
export const PROTOCOL_PALETTE: readonly string[] = [
  '#7FB4D8', // slate blue
  '#C9A96B', // brass
  '#A98FD1', // lilac
  '#6BBFB4', // sea
  '#D18F9E', // rose
  '#9DBF6B', // moss
  '#8FA1D1', // periwinkle
  '#B4B48C', // sage
];

// Deterministic kind → hue. A hash, not a lookup table, so a workflow
// published tomorrow gets its color with zero code change (registries
// over hardcoded paths — the palette is the only fixed data).
export function protocolHue(kind: string): string {
  // FNV-1a with an avalanche finish — a plain multiplicative roll
  // mod 8 sent ship-a-change and pr-train to the same slot.
  let h = 2166136261;
  for (let i = 0; i < kind.length; i++) {
    h ^= kind.charCodeAt(i);
    h = Math.imul(h, 16777619) >>> 0;
  }
  h ^= h >>> 15;
  h = Math.imul(h, 2246822519) >>> 0;
  h ^= h >>> 13;
  return PROTOCOL_PALETTE[(h >>> 0) % PROTOCOL_PALETTE.length] ?? PROTOCOL_PALETTE[0]!;
}
