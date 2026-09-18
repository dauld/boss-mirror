// Types for the dispatcher rule-registry surface (`GET /api/dispatcher/rules`,
// served by boss-dispatcher). Deserialized once at the fetch call site, per
// the repo convention (no shared-types lib).

/** One handler invocation inside a rule's `do` list. */
export type DispatcherRuleDo = Readonly<{
  handler: string;
  args: Readonly<Record<string, string>>;
}>;

/** A rule's clock-driven trigger (boss-dispatcher `RawSchedule`).
 *  `cadence` is the registry's token: `daily` | `weekly` | `biweekly` |
 *  `monthly` | `quarterly` | `annually` | `hourly` | `every-<n>-minutes`. */
export type DispatcherSchedule = Readonly<{
  cadence: string;
  anchor_date: string;
  business_calendar?: string | null;
}>;

/** One rule the dispatcher is ENFORCING: the `dispatcher_rules` row,
 *  plus the justification its authored file records. */
export type DispatcherRule = Readonly<{
  name: string;
  /** The NATS topic the rule listens for. A rule is triggered EITHER
   *  by an event OR by a schedule (boss-dispatcher rules/registry.rs
   *  RawRule: exactly one of the two), so a clock-driven rule carries
   *  no `on_event` at all — 14 of the 50 rows on the system of record,
   *  measured 2026-09-18, the shape the first nightly playground crawl
   *  found the cascade page throwing on (backlog ee86a789). */
  on_event?: string | null;
  /** The clock-driven trigger, present exactly when `on_event` is not. */
  schedule?: DispatcherSchedule | null;
  when: string | null;
  /** `do` is a reserved word in TS only as a statement — fine as a key. */
  do: ReadonlyArray<DispatcherRuleDo>;
  delay?: string | null;
  version: number;
  /** Registry status these rows were selected on — always `"active"`
   *  today, stated so a row says what it is. */
  status?: string;
  /** Why this reaction could not be a protocol consequence, from the
   *  rule's `infra/dispatcher/rules/` file. `null` when no file records
   *  it — check `authored` before reading that as "no reason given". */
  why?: string | null;
  /** Whether the authored registry holds a file for this rule at all.
   *  `false` = a reaction the system enforces that nobody wrote down —
   *  unless `source` names a tenant, whose rules no product file can. */
  authored?: boolean;
  /** Who declared the row: `"product"` (the authored directory, a
   *  migration, this editor) or `"tenant:<tenant_id>"` (a tenant's own
   *  `seeds/rules.toml`, backlog 458971ef). */
  source?: string;
}>;

/** Where the `why` values came from, so `why: null` everywhere can be
 *  told apart from an authored registry that could not be read. */
export type AuthoredRegistry = Readonly<{
  dir: string | null;
  rules: number;
  error: string | null;
}>;

/** A cascade edge that closes a loop but isn't a dispatcher rule — a
 *  jobs-api DAG consequence or an external counterparty. */
export type SystemEdge = Readonly<{
  from: string;
  to: string;
  /** `"jobs-api"` | `"external"`. */
  kind: string;
  label: string;
}>;

/** The full `/api/dispatcher/rules` payload. */
export type DispatcherRules = Readonly<{
  rules: ReadonlyArray<DispatcherRule>;
  /** handler name → event kinds it causes to be emitted (empty = sink). */
  handler_emits: Readonly<Record<string, ReadonlyArray<string>>>;
  system_edges: ReadonlyArray<SystemEdge>;
  /** `null` when the rules query itself failed (see `error`). */
  authored_registry?: AuthoredRegistry | null;
  /** Present only when the dispatcher couldn't read the registry table. */
  error?: string;
}>;
