// Types for the dispatcher rule-registry surface (`GET /api/dispatcher/rules`,
// served by boss-dispatcher). Deserialized once at the fetch call site, per
// the repo convention (no shared-types lib).

/** One handler invocation inside a rule's `do` list. */
export type DispatcherRuleDo = Readonly<{
  handler: string;
  args: Readonly<Record<string, string>>;
}>;

/** One dispatcher rule, verbatim from its `infra/dispatcher/rules/` file. */
export type DispatcherRule = Readonly<{
  name: string;
  on_event: string;
  when: string | null;
  /** `do` is a reserved word in TS only as a statement — fine as a key. */
  do: ReadonlyArray<DispatcherRuleDo>;
  delay?: string | null;
  version: number;
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
  /** Present only when the dispatcher couldn't read/parse the rules file. */
  error?: string;
}>;
