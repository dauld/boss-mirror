import { describe, expect, test } from 'bun:test';
import { buildCascade, describeTrigger, filterCascadeFromEvents, topicMatch, triggerTopics } from './cascadeToGraph';
import type { DispatcherRules } from './types';

describe('topicMatch', () => {
  test('exact, single-wildcard, trailing-wildcard', () => {
    expect(topicMatch('step.done.billing', 'step.done.billing')).toBe(true);
    expect(topicMatch('step.ready.*', 'step.ready.delegate-subjob')).toBe(true);
    expect(topicMatch('step.done.*', 'step.done.billing')).toBe(true);
    expect(topicMatch('step.done.*', 'step.done.billing.detail')).toBe(false);
    expect(topicMatch('step.>', 'step.done.billing.detail')).toBe(true);
    expect(topicMatch('inventory.item.consumed', 'inventory.item.received')).toBe(false);
  });
});

describe('buildCascade', () => {
  test('a self-feeding rule forms a highlighted cycle', () => {
    const data: DispatcherRules = {
      rules: [
        { name: 'loop', on_event: 'ev.x', when: null, do: [{ handler: 'h1', args: {} }], version: 1 },
      ],
      handler_emits: { h1: ['ev.x'] },
      system_edges: [],
    };
    const byId = new Map(buildCascade(data).nodes.map((n) => [n.id, n]));
    expect(byId.get('evt:ev.x')?.inCycle).toBe(true);
    expect(byId.get('rule:loop')?.inCycle).toBe(true);
    expect(byId.get('hdl:h1')?.inCycle).toBe(true);
  });

  test('system + wildcard edges close the DAG-advance loop', () => {
    // jobs.complete_step --emit--> jobs.step.completed --system--> step.ready.*
    //   --trigger--> marker --do--> jobs.complete_step   (a cycle)
    const data: DispatcherRules = {
      rules: [
        {
          name: 'marker',
          on_event: 'step.ready.*',
          when: null,
          do: [{ handler: 'jobs.complete_step', args: {} }],
          version: 1,
        },
      ],
      handler_emits: { 'jobs.complete_step': ['jobs.step.completed'] },
      system_edges: [
        { from: 'jobs.step.completed', to: 'step.ready.*', kind: 'jobs-api', label: 'readies dependents' },
      ],
    };
    const byId = new Map(buildCascade(data).nodes.map((n) => [n.id, n]));
    expect(byId.get('rule:marker')?.inCycle).toBe(true);
    expect(byId.get('hdl:jobs.complete_step')?.inCycle).toBe(true);
  });

  test('linear chain has no cycle; a wildcard match edge bridges topics', () => {
    const data: DispatcherRules = {
      rules: [
        { name: 'agg', on_event: 'metric.*', when: null, do: [{ handler: 'sink', args: {} }], version: 1 },
        { name: 'src', on_event: 'tick', when: null, do: [{ handler: 'emit_cpu', args: {} }], version: 1 },
      ],
      handler_emits: { emit_cpu: ['metric.cpu'], sink: [] },
      system_edges: [],
    };
    const g = buildCascade(data);
    const byId = new Map(g.nodes.map((n) => [n.id, n]));
    expect(byId.get('rule:agg')?.inCycle).toBe(false);
    // emit_cpu emits the concrete metric.cpu, which the metric.* trigger
    // covers — a `match` edge must bridge them.
    expect(g.edges.some((e) => e.kind === 'match' && e.source === 'evt:metric.cpu' && e.target === 'evt:metric.*')).toBe(true);
  });
});

describe('a handler no live rule invokes', () => {
  // Measured on the system of record 2026-09-23 (backlog ec40e269): 21 of
  // the 51 handlers the dispatcher build declares are invoked by no live
  // rule — the company-module handlers whose rules moved to the example
  // tenants' seeds — and drawing them put 42 of 158 nodes on the page
  // showing wiring no live protocol produces, the AR loop among them.
  const data: DispatcherRules = {
    rules: [{ name: 'spawn', on_event: 'step.done.x', when: null, do: [{ handler: 'jobs.spawn', args: {} }], version: 1 }],
    handler_emits: {
      'jobs.spawn': ['jobs.job.created'],
      'commerce.invoice.issue': ['commerce.invoice.created'],
    },
    system_edges: [
      { from: 'jobs.job.created', to: 'step.ready.*', kind: 'jobs-api', label: 'entry steps ready' },
      { from: 'commerce.invoice.created', to: 'commerce.invoice.paid', kind: 'external', label: 'settled' },
    ],
  };

  test('is not drawn, and neither is what only it would emit', () => {
    const ids = new Set(buildCascade(data).nodes.map((n) => n.id));
    expect(ids.has('hdl:jobs.spawn')).toBe(true);
    expect(ids.has('hdl:commerce.invoice.issue')).toBe(false);
    expect(ids.has('evt:commerce.invoice.created')).toBe(false);
  });

  test('a system edge from an event nothing drawn produces is not drawn', () => {
    const g = buildCascade(data);
    const ids = new Set(g.nodes.map((n) => n.id));
    expect(ids.has('evt:step.ready.*')).toBe(true);
    expect(ids.has('evt:commerce.invoice.paid')).toBe(false);
    expect(g.edges.every((e) => ids.has(e.source) && ids.has(e.target))).toBe(true);
  });
});

describe('filterCascadeFromEvents', () => {
  // a → r1 → h1 → (emit) b → r2 → h2(sink)
  const data: DispatcherRules = {
    rules: [
      { name: 'r1', on_event: 'a', when: null, do: [{ handler: 'h1', args: {} }], version: 1 },
      { name: 'r2', on_event: 'b', when: null, do: [{ handler: 'h2', args: {} }], version: 1 },
    ],
    handler_emits: { h1: ['b'], h2: [] },
    system_edges: [],
  };

  test('empty selection returns the full cascade unchanged', () => {
    const full = buildCascade(data);
    expect(filterCascadeFromEvents(full, [])).toBe(full);
  });

  test('forward cascade from a trigger keeps its whole downstream chain', () => {
    const f = filterCascadeFromEvents(buildCascade(data), ['a']);
    const ids = new Set(f.nodes.map((n) => n.id));
    expect(ids).toEqual(new Set(['evt:a', 'rule:r1', 'hdl:h1', 'evt:b', 'rule:r2', 'hdl:h2']));
    expect(f.edges.every((e) => ids.has(e.source) && ids.has(e.target))).toBe(true);
  });

  test('a downstream trigger excludes upstream-only nodes', () => {
    const f = filterCascadeFromEvents(buildCascade(data), ['b']);
    const ids = new Set(f.nodes.map((n) => n.id));
    expect(ids).toEqual(new Set(['evt:b', 'rule:r2', 'hdl:h2']));
    expect(ids.has('evt:a')).toBe(false);
    expect(ids.has('rule:r1')).toBe(false);
  });
});

describe('a scheduled rule (no on_event)', () => {
  // The dispatcher's registry is on_event XOR schedule (boss-dispatcher
  // rules/registry.rs RawRule): a clock-driven rule carries `schedule`
  // and NO `on_event`. Measured 2026-09-18 on the system of record: 14 of
  // 50 rows are shaped so, and the playground's real rows the same way.
  // The first nightly playground crawl (car 01180167) found the page
  // throwing `Cannot read properties of undefined (reading 'split')` on
  // them — topicMatch splitting an undefined trigger (backlog ee86a789).
  const data: DispatcherRules = {
    rules: [
      {
        name: 'sweep-daily',
        when: null,
        do: [{ handler: 'sweep', args: {} }],
        version: 1,
        schedule: { cadence: 'daily', anchor_date: '2026-09-09' },
      },
      { name: 'on-tick', on_event: 'tick.*', when: null, do: [{ handler: 'emit_tick', args: {} }], version: 1 },
    ],
    handler_emits: { sweep: ['tick.swept'], emit_tick: [] },
    system_edges: [],
  };

  test('builds without throwing and draws the rule with no trigger event', () => {
    const g = buildCascade(data);
    const byId = new Map(g.nodes.map((n) => [n.id, n]));
    expect(byId.get('rule:sweep-daily')?.sublabel).toBe('every day  ·  from 2026-09-09');
    expect(byId.has('evt:undefined')).toBe(false);
    // No edge into the rule at all: nothing precedes a clock.
    expect(g.edges.some((e) => e.target === 'rule:sweep-daily')).toBe(false);
    // Its handler's emit still bridges to the event-triggered rule.
    expect(g.edges.some((e) => e.kind === 'match' && e.source === 'evt:tick.swept' && e.target === 'evt:tick.*')).toBe(true);
  });

  test('the trigger list holds only real topics', () => {
    expect(triggerTopics(data.rules)).toEqual(['tick.*']);
  });

  test('a schedule is described in words, not as a missing topic', () => {
    expect(describeTrigger(data.rules[0]!)).toBe('every day  ·  from 2026-09-09');
    expect(describeTrigger({ ...data.rules[0]!, schedule: { cadence: 'every-15-minutes', anchor_date: '2026-09-09' } })).toBe('every 15 minutes  ·  from 2026-09-09');
    expect(describeTrigger(data.rules[1]!)).toBe('on tick.*');
    // Neither field — the registry refuses the row, but the page must not.
    expect(describeTrigger({ ...data.rules[1]!, on_event: undefined })).toBe('no trigger recorded');
  });
});
