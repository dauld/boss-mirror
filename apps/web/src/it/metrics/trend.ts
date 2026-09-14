// The codebase trend's read model — a projection of the daily
// `maintenance-codebase-metrics` packets, nothing else.
//
// David, 2026-09-11: "once we have code base analysis statistics we can
// start to get a sense for whether we are getting simpler and more
// reliable or more complex as we go." The measuring half landed first
// (infra/codebase-metrics.sh files `metadata.measured` and
// `metadata.landings` on a packet a day; the FIELDS and their limits
// are documented there, at the top of the file, and this module does
// not restate them). This is the reading half: one surface in the IT
// department that renders the series off those packets.
//
// TWO RATIOS ARE THE HEADLINE, because they are the two the founding
// ideas actually commit to (backlog 06048ade): delete:add per landing
// (`boss-codebase-shrinks`) and registry rows to code branches on kind
// (CLAUDE.md §9). Everything else the packet carries is context and is
// rendered as such.
//
// EVERY NUMBER HERE IS A FUNCTION OF THE PACKET FIELDS. The page adds
// nothing the record does not hold: a day's ratio is its own adds and
// deletes; the reading in words is computed from the same sums it
// prints; a null in the row (nothing added; no counter on the machine)
// stays a null with the row's own reason, never a zero. A failed read
// is a failure, never an empty series.
//
// THE SERIES IS THE UNION OF EVERY MEASURED PACKET. The first run
// backfilled the whole history; each later run carries only the
// landings since the previous packet's head (`measured.since`). One
// packet is therefore a window, and the trend is all of them joined on
// sha — which is also what makes a re-measured overlap harmless.

import { fetchRemote, type Remote } from '../../data/remote';

// ---------------------------------------------------------------------
// Types — the packet fields the page reads
// ---------------------------------------------------------------------

export type Landing = Readonly<{
  sha: string;
  at: string;
  subject: string;
  add: number;
  del: number;
  test_add: number;
  test_del: number;
}>;

export type AddDel = Readonly<{ add: number; del: number }>;

export type Window = Readonly<{
  adds: number;
  dels: number;
  net: number;
  landings: number;
  /** null when nothing was added — the script's own rule. */
  delete_add_pct: number | null;
  by_bucket: Readonly<Record<string, AddDel>>;
}>;

export type Totals = Readonly<{
  lines: number;
  prod_lines: number;
  test_lines: number;
  test_prod_pct: number | null;
  by_bucket: Readonly<Record<string, number>>;
}>;

export type LeakedSite = Readonly<{ file: string; line: number; scrutinee: string; literals: ReadonlyArray<string> }>;

export type Registry = Readonly<{
  rows: number;
  by_registry: Readonly<Record<string, number>>;
  /** null when the machine had no counter; the reason rides beside it. */
  code_branches_on_kind: number | null;
  code_branches_not_counted_why: string | null;
  code_branches_by_class: Readonly<Record<string, number>>;
  leaked_sites: ReadonlyArray<LeakedSite>;
}>;

export type Measured = Readonly<{
  at: string;
  ref: string | null;
  head: string;
  head_at: string | null;
  backfill: boolean;
  since: string | null;
  counts: Readonly<Record<string, number>>;
  /** `counts.crates_by_tier` — the four tiers of CLAUDE.md §Structure,
   *  nested inside `counts` in the row; empty when the row predates it. */
  crates_by_tier: Readonly<Record<string, number>>;
  window: Window;
  totals: Totals | null;
  registry: Registry | null;
}>;

export type MetricsPacket = Readonly<{
  id: string;
  title: string;
  measured: Measured;
  landings: ReadonlyArray<Landing>;
}>;

export type MetricsPage = Readonly<{
  packets: ReadonlyArray<MetricsPacket>;
  /** Packets of the kind that carry no measurement (a failed run). */
  unmeasured: number;
  total: number;
}>;

// ---------------------------------------------------------------------
// Parsing — once, at the fetch call site
// ---------------------------------------------------------------------

const rec = (v: unknown): Record<string, unknown> | null =>
  v !== null && typeof v === 'object' && !Array.isArray(v) ? (v as Record<string, unknown>) : null;
const str = (v: unknown): string | null => (typeof v === 'string' && v !== '' ? v : null);
const num = (v: unknown): number | null => (typeof v === 'number' && Number.isFinite(v) ? v : null);
const int = (v: unknown): number => num(v) ?? 0;

function numMap(v: unknown): Readonly<Record<string, number>> {
  const r = rec(v);
  if (!r) return {};
  return Object.fromEntries(Object.entries(r).flatMap(([k, x]) => (num(x) !== null ? [[k, x as number]] : [])));
}

function addDelMap(v: unknown): Readonly<Record<string, AddDel>> {
  const r = rec(v);
  if (!r) return {};
  return Object.fromEntries(
    Object.entries(r).flatMap(([k, x]) => {
      const b = rec(x);
      return b ? [[k, { add: int(b.add), del: int(b.del) }]] : [];
    }),
  );
}

function parseLanding(v: unknown): Landing | null {
  const r = rec(v);
  const sha = r ? str(r.sha) : null;
  const at = r ? str(r.at) : null;
  if (!r || !sha || !at) return null;
  return {
    sha,
    at,
    subject: str(r.subject) ?? '',
    add: int(r.add),
    del: int(r.del),
    test_add: int(r.test_add),
    test_del: int(r.test_del),
  };
}

function parseWindow(v: unknown): Window {
  const r = rec(v) ?? {};
  return {
    adds: int(r.adds),
    dels: int(r.dels),
    net: int(r.net),
    landings: int(r.landings),
    delete_add_pct: num(r.delete_add_pct),
    by_bucket: addDelMap(r.by_bucket),
  };
}

function parseTotals(v: unknown): Totals | null {
  const r = rec(v);
  if (!r) return null;
  return {
    lines: int(r.lines),
    prod_lines: int(r.prod_lines),
    test_lines: int(r.test_lines),
    test_prod_pct: num(r.test_prod_pct),
    by_bucket: numMap(r.by_bucket),
  };
}

function parseSites(v: unknown): ReadonlyArray<LeakedSite> {
  const sites = rec(v);
  const leaked = sites ? sites.leaked_policy : null;
  if (!Array.isArray(leaked)) return [];
  return leaked.flatMap((s) => {
    const r = rec(s);
    const file = r ? str(r.file) : null;
    if (!r || !file) return [];
    const lits = Array.isArray(r.literals) ? r.literals.flatMap((l) => (typeof l === 'string' ? [l] : [])) : [];
    return [{ file, line: int(r.line), scrutinee: str(r.scrutinee) ?? '', literals: lits }];
  });
}

function parseRegistry(v: unknown): Registry | null {
  const r = rec(v);
  if (!r) return null;
  return {
    rows: int(r.rows),
    by_registry: numMap(r.by_registry),
    code_branches_on_kind: num(r.code_branches_on_kind),
    code_branches_not_counted_why: str(r.code_branches_not_counted_why),
    code_branches_by_class: numMap(r.code_branches_by_class),
    leaked_sites: parseSites(r.code_branches_sites),
  };
}

function parseMeasured(v: unknown): Measured | null {
  const r = rec(v);
  const at = r ? str(r.at) : null;
  const head = r ? str(r.head) : null;
  if (!r || !at || !head) return null;
  return {
    at,
    ref: str(r.ref),
    head,
    head_at: str(r.head_at),
    backfill: r.backfill === true,
    since: str(r.since),
    counts: numMap(r.counts),
    crates_by_tier: numMap(rec(r.counts)?.crates_by_tier),
    window: parseWindow(r.window),
    totals: parseTotals(r.totals),
    registry: parseRegistry(r.registry),
  };
}

/** The jobs-API page for the kind: `{data, total}` or a bare array. */
export function parseMetricsPackets(raw: unknown): MetricsPage {
  const env = rec(raw);
  const list: unknown[] = Array.isArray(raw) ? raw : env && Array.isArray(env.data) ? env.data : [];
  const total = env ? (num(env.total) ?? list.length) : list.length;
  const packets = list.flatMap((j): MetricsPacket[] => {
    const r = rec(j);
    const id = r ? str(r.id) : null;
    const meta = r ? rec(r.metadata) : null;
    const measured = meta ? parseMeasured(meta.measured) : null;
    if (!r || !id || !measured) return [];
    const landings = meta && Array.isArray(meta.landings) ? meta.landings.flatMap((l) => parseLanding(l) ?? []) : [];
    return [{ id, title: str(r.title) ?? id, measured, landings }];
  });
  return { packets, unmeasured: list.length - packets.length, total };
}

/** The whole kind, newest first. Each run files one packet, so a page
 *  of `limit` covers `limit` days once the backfill has been joined by
 *  daily rows; the page reports `total` past what it read. */
export function loadMetricsPackets(limit: number): Promise<Exclude<Remote<MetricsPage>, { kind: 'loading' }>> {
  return fetchRemote(`/api/jobs?kind=maintenance-codebase-metrics&limit=${limit}`, parseMetricsPackets);
}

// ---------------------------------------------------------------------
// Derivations — pure functions of the fields above
// ---------------------------------------------------------------------

/** One card on the "codebase now" strip: a label, the number as text,
 *  and the one-line breakdown under it. */
export type StatCard = Readonly<{ k: string; v: string; sub: string }>;

/** THE CODEBASE NOW — the plain numbers off the newest row, before any
 *  reading of them. Feedback 9827c699 (David, 2026-09-14): "add a page
 *  to the IT department showing the Code base stats" — the trend page
 *  existed as a Design tab and opened on the delete:add verdict, so the
 *  stats a person wanted were three sections down and one tab in. This
 *  strip is those stats; the reading follows it. Pure over the row so
 *  the page cannot invent a number the row does not carry: a missing
 *  totals/registry block yields no card rather than a zero. */
export function statsStrip(m: Measured): ReadonlyArray<StatCard> {
  const n = (v: number): string => v.toLocaleString('en-US');
  const cards: StatCard[] = [];
  const c = m.counts;
  if (typeof c.crates === 'number') {
    const tiers = Object.entries(m.crates_by_tier)
      .sort(([, a], [, b]) => b - a)
      .map(([k, v]) => `${v} ${k}`)
      .join(' · ');
    cards.push({ k: 'crates', v: n(c.crates), sub: tiers || 'by tier: not in this row' });
  }
  if (typeof c.rust_files === 'number' || typeof c.web_files === 'number') {
    cards.push({
      k: 'source files',
      v: n((c.rust_files ?? 0) + (c.web_files ?? 0)),
      sub: `${n(c.rust_files ?? 0)} rust · ${n(c.web_files ?? 0)} web`,
    });
  }
  if (m.totals) {
    const t = m.totals;
    cards.push({
      k: 'lines',
      v: n(t.lines),
      sub: `${n(t.prod_lines)} prod · ${n(t.test_lines)} test${t.test_prod_pct === null ? '' : ` (${t.test_prod_pct}% of prod)`}`,
    });
  }
  if (typeof c.lints === 'number' || typeof c.migrations === 'number') {
    cards.push({
      k: 'gate lints · migrations',
      v: `${n(c.lints ?? 0)} · ${n(c.migrations ?? 0)}`,
      sub: 'checks every car passes · schema files, append-only',
    });
  }
  if (m.registry) {
    const r = m.registry;
    const kinds = Object.entries(r.by_registry)
      .sort(([, a], [, b]) => b - a)
      .map(([k, v]) => `${v} ${k.replace(/_/g, ' ')}`)
      .join(' · ');
    cards.push({ k: 'registry rows', v: n(r.rows), sub: kinds || 'by registry: not in this row' });
    cards.push({
      k: 'code branches on kind',
      v: r.code_branches_on_kind === null ? 'not counted' : n(r.code_branches_on_kind),
      sub:
        r.code_branches_on_kind === null
          ? r.code_branches_not_counted_why ?? 'the machine had no counter'
          : 'match arms a registry row should have replaced (CLAUDE.md §9)',
    });
  }
  return cards;
}

/** The packet measured last — by `measured.at`, not by API order. */
export function newestMeasured(packets: ReadonlyArray<MetricsPacket>): MetricsPacket | null {
  return packets.reduce<MetricsPacket | null>((best, p) => (best === null || p.measured.at > best.measured.at ? p : best), null);
}

/** Every landing any packet carried, once per sha, newest first. */
export function mergeLandings(packets: ReadonlyArray<MetricsPacket>): ReadonlyArray<Landing> {
  const bySha = new Map<string, Landing>();
  packets.forEach((p) => p.landings.forEach((l) => bySha.set(l.sha, l)));
  return [...bySha.values()].sort((a, b) => (a.at < b.at ? 1 : a.at > b.at ? -1 : 0));
}

/** The script's rule, exactly: int(del * 100 / add + 0.5); null when nothing was added. */
export function deleteAddPct(add: number, del: number): number | null {
  return add > 0 ? Math.floor((del * 100) / add + 0.5) : null;
}

export type DayRow = Readonly<{
  day: string;
  landings: number;
  add: number;
  del: number;
  net: number;
  testAdd: number;
  testDel: number;
  deleteAddPct: number | null;
}>;

const dayOf = (at: string): string => at.slice(0, 10);

/** Landings folded into UTC days, oldest first. */
export function perDay(landings: ReadonlyArray<Landing>): ReadonlyArray<DayRow> {
  const days = landings.reduce<Map<string, { n: number; add: number; del: number; ta: number; td: number }>>((m, l) => {
    const d = dayOf(l.at);
    const cur = m.get(d) ?? { n: 0, add: 0, del: 0, ta: 0, td: 0 };
    m.set(d, { n: cur.n + 1, add: cur.add + l.add, del: cur.del + l.del, ta: cur.ta + l.test_add, td: cur.td + l.test_del });
    return m;
  }, new Map());
  return [...days.entries()]
    .sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0))
    .map(([day, s]) => ({
      day,
      landings: s.n,
      add: s.add,
      del: s.del,
      net: s.add - s.del,
      testAdd: s.ta,
      testDel: s.td,
      deleteAddPct: deleteAddPct(s.add, s.del),
    }));
}

/** The day rows inside the last `days` days ending on the newest row;
 *  `null` keeps them all. The chart needs this because the backfill
 *  reaches the initial commit, whose one landing would set the scale
 *  for everything after it. */
export function lastDays(rows: ReadonlyArray<DayRow>, days: number | null): ReadonlyArray<DayRow> {
  const last = rows[rows.length - 1];
  if (days === null || !last) return rows;
  const fromMs = Date.parse(`${last.day}T00:00:00Z`) - (days - 1) * 86_400_000;
  const from = new Date(fromMs).toISOString().slice(0, 10);
  return rows.filter((r) => r.day >= from);
}

export type WindowSum = Readonly<{
  days: number;
  from: string | null;
  to: string | null;
  landings: number;
  add: number;
  del: number;
  net: number;
  deleteAddPct: number | null;
}>;

/** The last `days` UTC days of the series, ending on the newest
 *  landing's day (not today: a stale head must not read as a quiet
 *  week, so the window follows the record, not the clock). */
export function recentWindow(landings: ReadonlyArray<Landing>, days: number): WindowSum {
  const empty: WindowSum = { days, from: null, to: null, landings: 0, add: 0, del: 0, net: 0, deleteAddPct: null };
  if (landings.length === 0) return empty;
  const to = landings.reduce((best, l) => (dayOf(l.at) > best ? dayOf(l.at) : best), dayOf(landings[0]!.at));
  const fromMs = Date.parse(`${to}T00:00:00Z`) - (days - 1) * 86_400_000;
  const from = new Date(fromMs).toISOString().slice(0, 10);
  const inside = landings.filter((l) => dayOf(l.at) >= from && dayOf(l.at) <= to);
  const add = inside.reduce((s, l) => s + l.add, 0);
  const del = inside.reduce((s, l) => s + l.del, 0);
  const first = inside.reduce((best, l) => (dayOf(l.at) < best ? dayOf(l.at) : best), to);
  return { days, from: first, to, landings: inside.length, add, del, net: add - del, deleteAddPct: deleteAddPct(add, del) };
}

/** Registry rows per code branch on kind, one decimal; null when the
 *  branch count is unknown or zero (a ratio over zero is not a number). */
export function registryRatio(rows: number, branches: number | null): number | null {
  return branches === null || branches <= 0 ? null : Math.round((rows / branches) * 10) / 10;
}

/** Days from the head landing to the measurement, one decimal. Large
 *  and growing across packets means a stale converge; large and steady
 *  with no newer landings anywhere means a quiet week. */
export function headGapDays(headAt: string | null, measuredAt: string): number | null {
  if (!headAt) return null;
  const ms = Date.parse(measuredAt) - Date.parse(headAt);
  return Number.isFinite(ms) ? Math.round(ms / 8_640_000) / 10 : null;
}

export type Verdict = 'more complex' | 'simpler' | 'unchanged' | 'no landings' | 'not counted' | 'first measurement';

export type Baseline = Readonly<{ at: string; rowsPerBranch: number }>;

export type Reading = Readonly<{
  volume: Readonly<{ verdict: Verdict; text: string; window: WindowSum }>;
  ratio: Readonly<{
    verdict: Verdict;
    text: string;
    rows: number;
    branches: number | null;
    rowsPerBranch: number | null;
    /** The earliest counted measurement the ratio is read against. */
    since: Baseline | null;
  }>;
}>;

const signed = (n: number): string => (n > 0 ? `+${n.toLocaleString('en-US')}` : n.toLocaleString('en-US'));

/** The earliest packet other than `newest` whose ratio is a number —
 *  the baseline a ratio trend is read against. */
function earliestCounted(packets: ReadonlyArray<MetricsPacket>, newest: MetricsPacket): Baseline | null {
  return packets
    .filter((p) => p.id !== newest.id)
    .flatMap((p): Baseline[] => {
      const reg = p.measured.registry;
      const r = reg ? registryRatio(reg.rows, reg.code_branches_on_kind) : null;
      return r === null ? [] : [{ at: p.measured.at, rowsPerBranch: r }];
    })
    .reduce<Baseline | null>((best, c) => (best === null || c.at < best.at ? c : best), null);
}

/** The reading in words, computed from the same sums the page prints.
 *  By VOLUME: the recent window's net lines. By RATIO: registry rows
 *  per code branch on kind in core, from the newest row, read against
 *  the earliest measurement that counted — one measurement is a level,
 *  not a trend, and says so. Which ratios count as "simpler" is a
 *  choice (the packet says so); these two are the ones the founding
 *  ideas commit to, and the words say which reading each verdict came
 *  from. */
export function reading(
  newest: MetricsPacket,
  landings: ReadonlyArray<Landing>,
  days: number,
  packets: ReadonlyArray<MetricsPacket>,
): Reading {
  const w = recentWindow(landings, days);
  const volumeVerdict: Verdict =
    w.landings === 0 ? 'no landings' : w.net > 0 ? 'more complex' : w.net < 0 ? 'simpler' : 'unchanged';
  const pct = w.deleteAddPct === null ? 'nothing added' : `deleting ${w.deleteAddPct}% of what it adds`;
  const volumeText =
    w.landings === 0
      ? `no landings in the last ${days} days of the series`
      : `${signed(w.net)} lines over ${w.landings} landing${w.landings === 1 ? '' : 's'} in the last ${w.days} day${w.days === 1 ? '' : 's'} (${w.from} to ${w.to}), ${pct}`;

  const reg = newest.measured.registry;
  const rows = reg?.rows ?? 0;
  const branches = reg?.code_branches_on_kind ?? null;
  const rowsPerBranch = registryRatio(rows, branches);
  const since = rowsPerBranch === null ? null : earliestCounted(packets, newest);
  const level =
    `${rows} registry rows to ${branches ?? 'n/a'} code branch${branches === 1 ? '' : 'es'} on kind in core` +
    (rowsPerBranch === null ? '' : ` — ${rowsPerBranch} rows of behaviour as data for every match still in code`);
  const ratioVerdict: Verdict =
    branches === null
      ? 'not counted'
      : since === null || rowsPerBranch === null
        ? 'first measurement'
        : rowsPerBranch > since.rowsPerBranch
          ? 'simpler'
          : rowsPerBranch < since.rowsPerBranch
            ? 'more complex'
            : 'unchanged';
  const direction = ratioVerdict === 'simpler' ? 'up from' : ratioVerdict === 'more complex' ? 'down from' : 'level with';
  const ratioText =
    branches === null
      ? `${rows} registry rows; code branches on kind were not counted — ${reg?.code_branches_not_counted_why ?? 'no reason recorded'}`
      : since === null
        ? `${level}; no earlier measurement to compare against yet`
        : `${level}, ${direction} ${since.rowsPerBranch} on ${since.at.slice(0, 10)}`;
  return {
    volume: { verdict: volumeVerdict, text: volumeText, window: w },
    ratio: { verdict: ratioVerdict, text: ratioText, rows, branches, rowsPerBranch, since },
  };
}

// ---------------------------------------------------------------------
// Chart geometry — a diverging bar per day, net lines
// ---------------------------------------------------------------------

export type Bar = Readonly<{ day: string; net: number; x: number; y: number; w: number; h: number; row: DayRow }>;

export type BarChart = Readonly<{ w: number; h: number; baseline: number; max: number; bars: ReadonlyArray<Bar> }>;

/** Net lines per day as bars off a shared baseline: growth rises,
 *  shrinkage hangs below, both scaled to the largest magnitude in the
 *  table so the two directions are comparable by eye. */
export function barGeometry(rows: ReadonlyArray<DayRow>, w: number, h: number): BarChart {
  const pad = 2;
  const max = rows.reduce((m, r) => Math.max(m, Math.abs(r.net)), 0);
  const up = rows.some((r) => r.net > 0);
  const down = rows.some((r) => r.net < 0);
  // The baseline sits where the data needs it: centred only when both
  // directions occur, otherwise at the foot (or head) of the plot.
  const baseline = up && down ? h / 2 : down ? pad : h - pad;
  const room = up && down ? h / 2 - pad : h - 2 * pad;
  const slot = rows.length > 0 ? w / rows.length : w;
  const bw = Math.max(1, slot - 1);
  const bars = rows.map((r, i) => {
    const len = max > 0 ? (Math.abs(r.net) / max) * room : 0;
    return {
      day: r.day,
      net: r.net,
      x: i * slot,
      w: bw,
      y: r.net >= 0 ? baseline - len : baseline,
      h: len,
      row: r,
    };
  });
  return { w, h, baseline, max, bars };
}
