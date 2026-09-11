// Types for the dispatcher rule-registry surface (`GET /api/dispatcher/rules`,
// served by boss-dispatcher). Deserialized once at the fetch call site, per
// the repo convention (no shared-types lib).

/** One handler invocation inside a rule's `do` list. */
export type DispatcherRuleDo = Readonly<{
  handler: string;
  args: Readonly<Record<string, string>>;
}>;

/** One rule the dispatcher is ENFORCING: the `dispatcher_rules` row,
 *  plus the justification its authored file records. */
export type DispatcherRule = Readonly<{
  name: string;
  on_event: string;
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
   *  `false` = a reaction the system enforces that nobody wrote down. */
  authored?: boolean;
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
