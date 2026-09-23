// Pure transform: the /api/dispatcher/rules payload → a directed graph of
// the reactive cascade. Three node kinds (event, rule, handler) and five
// edge kinds:
//   trigger  on_event → rule        (a rule listens for its topic)
//   do       rule → handler         (the rule fires the handler)
//   emit     handler → event        (the handler causes that event)
//   system   event → event          (jobs-api / external consequence)
//   match    event → event          (a produced topic activates a trigger
//                                     across a NATS wildcard, e.g.
//                                     step.done.* → step.done.billing)
//
// Loops close because event nodes are shared by topic string (an emit and
// an on_event of the same name are one node) and via `match` edges across
// wildcards. `inCycle` is filled from Tarjan SCCs so the page can light up
// the feedback cycles.
//
// Only what a live rule invokes is drawn (backlog ec40e269). The feed's
// `handler_emits` is the dispatcher BUILD's roster — every handler the
// binary registers — and on 2026-09-23 21 of its 51 were invoked by no
// live rule (the company-module handlers whose rules moved to the example
// tenants' seeds), so drawing the roster put 42 of 158 nodes on the page
// showing wiring no live protocol produces. A handler is drawn when a rule
// fires it; an event when a rule listens for it or a drawn handler emits
// it; a system edge when its source event is drawn.

import type { DispatcherRule, DispatcherRules, SystemEdge } from './types';

export type CascadeNodeKind = 'event' | 'rule' | 'handler';
export type CascadeEdgeKind = 'trigger' | 'do' | 'emit' | 'system' | 'match';

export type CascadeNode = Readonly<{
  id: string;
  kind: CascadeNodeKind;
  label: string;
  sublabel?: string;
  /** The underlying event string / rule name / handler name, for the inspector. */
  ref: string;
  inCycle: boolean;
}>;

export type CascadeEdge = Readonly<{
  id: string;
  source: string;
  target: string;
  kind: CascadeEdgeKind;
  label?: string;
  inCycle: boolean;
}>;

export type Cascade = Readonly<{ nodes: CascadeNode[]; edges: CascadeEdge[] }>;

/** NATS-style topic match: `*` matches exactly one token, `>` matches one
 *  or more trailing tokens. `pattern` is the subscription, `topic` the
 *  concrete (or wildcard) subject. */
export function topicMatch(pattern: string, topic: string): boolean {
  const p = pattern.split('.');
  const t = topic.split('.');
  for (let i = 0; i < p.length; i++) {
    if (p[i] === '>') return true;
    if (i >= t.length) return false;
    if (p[i] === '*') continue;
    if (p[i] !== t[i]) return false;
  }
  return p.length === t.length;
}

const EVT = (e: string): string => `evt:${e}`;
const RULE = (n: string): string => `rule:${n}`;
const HDL = (h: string): string => `hdl:${h}`;

/** The distinct topics rules listen for, sorted. A scheduled rule has
 *  no topic (on_event XOR schedule in the registry), so it contributes
 *  nothing here — the first nightly playground crawl (car 01180167,
 *  backlog ee86a789) found `undefined` in this set reaching
 *  `topicMatch`, which split it and threw. */
export function triggerTopics(rules: ReadonlyArray<DispatcherRule>): string[] {
  return [...new Set(rules.flatMap((r) => (r.on_event ? [r.on_event] : [])))].sort();
}

/** What fires the rule, in words: `on <topic>` for an event-triggered
 *  rule, the cadence for a scheduled one, and an honest "no trigger
 *  recorded" for a row carrying neither (the registry refuses such a
 *  row at load; the page must not). */
export function describeTrigger(rule: Pick<DispatcherRule, 'on_event' | 'schedule'>): string {
  if (rule.on_event) return `on ${rule.on_event}`;
  const s = rule.schedule;
  if (!s) return 'no trigger recorded';
  const minutes = /^every-(\d+)-minutes$/.exec(s.cadence)?.[1];
  const every = minutes ? `every ${minutes} minutes` : (CADENCE_WORDS[s.cadence] ?? `every ${s.cadence}`);
  return `${every}  ·  from ${s.anchor_date}`;
}

const CADENCE_WORDS: Readonly<Record<string, string>> = {
  daily: 'every day',
  weekly: 'every week',
  biweekly: 'every two weeks',
  monthly: 'every month',
  quarterly: 'every quarter',
  annually: 'every year',
  hourly: 'every hour',
};

/** The handlers a rule fires, and what each of them emits — the part of
 *  the build's roster the live rules actually wire. */
export function invokedEmits(data: DispatcherRules): Record<string, ReadonlyArray<string>> {
  const roster = data.handler_emits ?? {};
  const invoked = new Set((data.rules ?? []).flatMap((r) => r.do.map((d) => d.handler)));
  return Object.fromEntries([...invoked].map((h) => [h, roster[h] ?? []]));
}

export function buildCascade(data: DispatcherRules): Cascade {
  const rules = data.rules ?? [];
  const emits = invokedEmits(data);

  // Universes.
  const eventSet = new Set<string>(triggerTopics(rules));
  for (const list of Object.values(emits)) for (const e of list) eventSet.add(e);
  // A system edge is drawn once its source is; one may feed another, so
  // take them to a fixpoint (the list is a handful long).
  const systemEdges: SystemEdge[] = [];
  let pending: SystemEdge[] = [...(data.system_edges ?? [])];
  for (let grew = true; grew; ) {
    grew = false;
    const next: SystemEdge[] = [];
    for (const se of pending) {
      if (eventSet.has(se.from)) {
        systemEdges.push(se);
        eventSet.add(se.to);
        grew = true;
      } else next.push(se);
    }
    pending = next;
  }
  const handlerSet = new Set<string>(Object.keys(emits));

  // Nodes (cycle flags filled after edges).
  type RawNode = Omit<CascadeNode, 'inCycle'>;
  const nodes: RawNode[] = [];
  for (const e of eventSet) nodes.push({ id: EVT(e), kind: 'event', label: e, ref: e });
  for (const r of rules) {
    const sub = `${describeTrigger(r)}${r.when ? '  ·  when ⚲' : ''}`;
    nodes.push({ id: RULE(r.name), kind: 'rule', label: r.name, sublabel: sub, ref: r.name });
  }
  for (const h of handlerSet) {
    const n = (emits[h] ?? []).length;
    nodes.push({
      id: HDL(h),
      kind: 'handler',
      label: h,
      sublabel: n ? `emits ${n}` : 'sink',
      ref: h,
    });
  }

  // Edges.
  type RawEdge = Omit<CascadeEdge, 'inCycle'>;
  const edges: RawEdge[] = [];
  for (const r of rules) {
    // A scheduled rule is a source in the graph: the clock is not an
    // event node, so it gets no trigger edge.
    if (r.on_event) edges.push({ id: `t:${r.name}`, source: EVT(r.on_event), target: RULE(r.name), kind: 'trigger' });
    r.do.forEach((d, i) =>
      edges.push({ id: `d:${r.name}:${i}`, source: RULE(r.name), target: HDL(d.handler), kind: 'do' }),
    );
  }
  for (const [h, list] of Object.entries(emits)) {
    for (const e of list) {
      edges.push({ id: `e:${h}:${e}`, source: HDL(h), target: EVT(e), kind: 'emit' });
    }
  }
  for (const se of systemEdges) {
    edges.push({
      id: `s:${se.from}->${se.to}`,
      source: EVT(se.from),
      target: EVT(se.to),
      kind: 'system',
      label: se.label,
    });
  }
  // Wildcard loop-closure: a produced topic that activates a rule's trigger
  // when they aren't the same string (e.g. system 'step.done.*' → the
  // concrete 'step.done.billing' on_event).
  const produced = new Set<string>();
  for (const list of Object.values(emits)) for (const e of list) produced.add(e);
  for (const se of systemEdges) produced.add(se.to);
  const triggers = new Set(triggerTopics(rules));
  for (const p of produced) {
    for (const trg of triggers) {
      if (p === trg) continue;
      if (topicMatch(p, trg) || topicMatch(trg, p)) {
        edges.push({ id: `m:${p}->${trg}`, source: EVT(p), target: EVT(trg), kind: 'match' });
      }
    }
  }

  // Cycle members + their SCC ids.
  const { members, sccOf } = detectCycleMembers(
    nodes.map((n) => n.id),
    edges,
  );

  return {
    nodes: nodes.map((n) => ({ ...n, inCycle: members.has(n.id) })),
    edges: edges.map((e) => ({
      ...e,
      inCycle:
        members.has(e.source) &&
        members.has(e.target) &&
        sccOf.get(e.source) === sccOf.get(e.target),
    })),
  };
}

/** Narrow a cascade to the forward-reachable subgraph from one or more
 *  trigger events (by event `ref` — the on_event topic). Follows edges in
 *  direction (trigger → rule → handler → emit → event → match/system → …),
 *  so you see exactly what firing those events cascades into. Cycle flags
 *  are preserved from the full graph. An empty selection returns the
 *  cascade unchanged (the full view). */
export function filterCascadeFromEvents(
  cascade: Cascade,
  eventRefs: ReadonlyArray<string>,
): Cascade {
  if (eventRefs.length === 0) return cascade;
  const refs = new Set(eventRefs);
  const starts = cascade.nodes
    .filter((n) => n.kind === 'event' && refs.has(n.ref))
    .map((n) => n.id);
  const adj = new Map<string, string[]>();
  for (const e of cascade.edges) {
    const out = adj.get(e.source);
    if (out) out.push(e.target);
    else adj.set(e.source, [e.target]);
  }
  const reachable = new Set<string>(starts);
  const queue = [...starts];
  while (queue.length > 0) {
    const v = queue.shift()!;
    for (const w of adj.get(v) ?? []) {
      if (!reachable.has(w)) {
        reachable.add(w);
        queue.push(w);
      }
    }
  }
  return {
    nodes: cascade.nodes.filter((n) => reachable.has(n.id)),
    edges: cascade.edges.filter((e) => reachable.has(e.source) && reachable.has(e.target)),
  };
}

/** Tarjan SCC. Returns the nodes that sit in a non-trivial strongly-connected
 *  component (or a self-loop) — i.e. are part of a feedback cycle — plus each
 *  node's component id so edge highlighting can stay intra-component. */
function detectCycleMembers(
  nodeIds: string[],
  edges: ReadonlyArray<{ source: string; target: string }>,
): { members: Set<string>; sccOf: Map<string, number> } {
  const adj = new Map<string, string[]>();
  for (const id of nodeIds) adj.set(id, []);
  const selfLoop = new Set<string>();
  for (const e of edges) {
    const out = adj.get(e.source);
    if (!out || !adj.has(e.target)) continue;
    out.push(e.target);
    if (e.source === e.target) selfLoop.add(e.source);
  }

  let index = 0;
  const idx = new Map<string, number>();
  const low = new Map<string, number>();
  const onStack = new Set<string>();
  const stack: string[] = [];
  const sccOf = new Map<string, number>();
  const sccSize = new Map<number, number>();
  let sccId = 0;

  const strongconnect = (v: string): void => {
    idx.set(v, index);
    low.set(v, index);
    index++;
    stack.push(v);
    onStack.add(v);
    for (const w of adj.get(v) ?? []) {
      if (!idx.has(w)) {
        strongconnect(w);
        low.set(v, Math.min(low.get(v)!, low.get(w)!));
      } else if (onStack.has(w)) {
        low.set(v, Math.min(low.get(v)!, idx.get(w)!));
      }
    }
    if (low.get(v) === idx.get(v)) {
      const comp: string[] = [];
      let w: string;
      do {
        w = stack.pop()!;
        onStack.delete(w);
        sccOf.set(w, sccId);
        comp.push(w);
      } while (w !== v);
      sccSize.set(sccId, comp.length);
      sccId++;
    }
  };

  for (const id of nodeIds) if (!idx.has(id)) strongconnect(id);

  const members = new Set<string>();
  for (const id of nodeIds) {
    const s = sccOf.get(id);
    if (s !== undefined && ((sccSize.get(s) ?? 0) > 1 || selfLoop.has(id))) members.add(id);
  }
  return { members, sccOf };
}
