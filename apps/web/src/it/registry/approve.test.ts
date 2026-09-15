// The Drift tab's approve — car 3b of 8f4e9cc0. The four decisions,
// pinned as pure functions the way abort.test.ts pins the Abort control:
//   kinds      — one control per adrift KIND, grouping the packet's
//                per-field rows; the fields ride into the confirmation.
//   mode       — which control a kind gets, decided by the verb's OWN
//                last answer for that kind (the history walk on the
//                host is the only machine-run judgement of "the tree
//                never said this"), never by a guess off the excerpt.
//   body       — the ops-request packet, in the shape `boss ops` files
//                (crates/orchestrators/boss-cli/src/ops_request.rs), with
//                the marker and the provenance this surface adds.
//   authority  — the role the ops-request `execute` step names admits
//                the viewer, or the control is disabled with the role
//                named (abortAuthority's shape).
import { describe, expect, test } from 'bun:test';

import {
  adriftKinds,
  approveAuthority,
  approveBody,
  executeAuthorityRole,
  forceConfirmed,
  latestFor,
  modeFor,
  parsePublishRequests,
  PUBLISH_HOST,
  PUBLISH_REQUESTS_QUERY,
  PUBLISH_VERB,
  REQUESTED_FROM,
  rowState,
  supersededFieldCount,
  type PublishRequest,
} from './approve';
import type { FieldDrift } from './drift';

const row = (over: Partial<FieldDrift>): FieldDrift => ({
  kind: 'ship-a-change',
  field: 'description',
  live_version: 31,
  at: 432,
  tree_len: 822,
  live_len: 1544,
  tree_window: '…Name the feedback packet this change answers…',
  live_window: '…OPERATOR EDIT 2026-08-27, deliberate and load-bearing…',
  ...over,
});

// An ops-request as the runner leaves it (measured on 1c0a02da,
// 2026-09-15): the answer is the `execute` step's metadata.
function request(over: {
  id?: string;
  args?: string[];
  status?: 'open' | 'closed';
  opened_at?: string;
  exit_code?: string | null;
  disposition?: 'answered' | 'refused' | null;
  verb?: string;
  requested_from?: string | null;
  output?: string;
  /** The execute step's `completed_at`; null = the step carries none. */
  answered_at?: string | null;
}): Record<string, unknown> {
  const disposition = over.disposition === undefined ? 'answered' : over.disposition;
  const execute: Record<string, unknown> = { authority_role: 'platform-admin' };
  if (disposition !== null) {
    execute.disposition = disposition;
    execute.output = over.output ?? 'publish-workflow: something';
    execute.runner_host = 'boss-gcp';
    if (over.exit_code !== null) execute.exit_code = over.exit_code ?? '0';
  }
  const answered_at = over.answered_at === undefined ? '2026-09-15T21:03:12.123456Z' : over.answered_at;
  const metadata: Record<string, unknown> = {
    host: 'boss-gcp',
    verb: over.verb ?? PUBLISH_VERB,
    args: over.args ?? ['ship-a-change'],
    opened_at: over.opened_at ?? '2026-09-15T21:00:00+00:00',
  };
  if (over.requested_from !== null) metadata.requested_from = over.requested_from ?? REQUESTED_FROM;
  return {
    id: over.id ?? 'req-1',
    kind: 'ops-request',
    title: 'publish-workflow ship-a-change on boss-gcp',
    status: over.status ?? 'closed',
    metadata,
    steps: [
      { spec_slug: 'filed', status: 'completed', metadata: {} },
      {
        spec_slug: 'execute',
        status: disposition === null ? 'ready' : 'completed',
        metadata: execute,
        ...(disposition !== null && answered_at !== null ? { completed_at: answered_at } : {}),
      },
    ],
  };
}

describe('adriftKinds', () => {
  test('groups the per-field rows by kind, in first-seen order, fields kept', () => {
    const fields = [
      row({ kind: 'maintenance-sweep', live_version: 2 }),
      row({ kind: 'ship-a-change', field: 'label' }),
      row({ kind: 'ship-a-change', field: 'description' }),
    ];
    expect(adriftKinds(fields).map((k) => [k.kind, k.fields.map((f) => f.field), k.live_version])).toEqual([
      ['maintenance-sweep', ['description'], 2],
      ['ship-a-change', ['label', 'description'], 31],
    ]);
    expect(adriftKinds([])).toEqual([]);
  });
});

describe('parsePublishRequests', () => {
  test('keeps only publish-workflow requests and reads the answer off the execute step', () => {
    const page = {
      data: [
        request({ id: 'a', exit_code: '6', output: 'publish-workflow: REFUSED — the live row carries what the tree never said: description' }),
        request({ id: 'b', verb: 'uptime', args: [] }),
        request({ id: 'c', status: 'open', disposition: null, args: ['maintenance-sweep'] }),
        request({ id: 'd', args: ['maintenance-sweep', '--force-tree'], requested_from: null }),
        request({ id: 'e', args: ['maintenance-files-gc', '--check'], exit_code: '4' }),
      ],
      total: 5,
    };
    const got = parsePublishRequests(page);
    expect(got.map((r) => r.id)).toEqual(['a', 'c', 'd', 'e']);
    expect(got[0]).toMatchObject({
      workflow_kind: 'ship-a-change',
      mode: 'plain',
      status: 'closed',
      exit_code: '6',
      disposition: 'answered',
      requested_from: 'drift-tab',
    });
    expect(got[0]?.output).toContain('never said');
    // In flight: no disposition, no exit — and never read as answered.
    expect(got[1]).toMatchObject({ workflow_kind: 'maintenance-sweep', status: 'open', disposition: null, exit_code: null });
    // A request `boss ops` filed from a terminal is still this kind's
    // answer — the marker is provenance, not a filter.
    expect(got[2]).toMatchObject({ mode: 'force', requested_from: null });
    // A terminal rehearsal (25cb2f71, 2026-09-15) is a check, not a publish.
    expect(got[3]).toMatchObject({ mode: 'check', exit_code: '4' });
  });

  test('the query is metadata containment on the verb, not a page of every verb', () => {
    expect(PUBLISH_REQUESTS_QUERY).toBe('/api/jobs?kind=ops-request&metadata=%7B%22verb%22%3A%22publish-workflow%22%7D');
  });

  test('a bare array and a malformed row are handled, never thrown on', () => {
    expect(parsePublishRequests([request({}), { id: 'x' }, null])).toHaveLength(1);
    expect(parsePublishRequests({ nope: true })).toEqual([]);
  });
});

describe('latestFor and modeFor', () => {
  const reqs: ReadonlyArray<PublishRequest> = parsePublishRequests([
    request({ id: 'old', opened_at: '2026-09-15T10:00:00+00:00', exit_code: '6' }),
    request({ id: 'new', opened_at: '2026-09-15T12:00:00+00:00', exit_code: '0' }),
    request({ id: 'other', args: ['maintenance-sweep'], exit_code: '5' }),
  ]);

  test('the latest request for a kind is by opened_at, not API order', () => {
    expect(latestFor(reqs, 'ship-a-change')?.id).toBe('new');
    expect(latestFor(reqs, 'maintenance-sweep')?.id).toBe('other');
    expect(latestFor(reqs, 'design-doc')).toBeNull();
  });

  test('no request yet, or any answer but a refusal-6, is the plain publish', () => {
    expect(modeFor(null)).toEqual({ kind: 'plain' });
    expect(modeFor(latestFor(reqs, 'ship-a-change'))).toEqual({ kind: 'plain' });
    expect(modeFor(latestFor(reqs, 'maintenance-sweep'))).toEqual({ kind: 'plain' });
  });

  test('a request still open holds the control: no twin is filed', () => {
    const [open] = parsePublishRequests([request({ id: 'c', status: 'open', disposition: null })]);
    expect(modeFor(open ?? null)).toEqual({ kind: 'pending', request_id: 'c' });
  });

  test('exit 6 — the live row carries what the tree never said — is the force mode, with its packet', () => {
    const [r6] = parsePublishRequests([request({ id: 'r6', exit_code: '6' })]);
    expect(modeFor(r6 ?? null)).toEqual({ kind: 'force', request_id: 'r6' });
    // A refusal by the RUNNER (outside the allowlist) is not exit 6.
    const [refused] = parsePublishRequests([request({ id: 'rf', disposition: 'refused', exit_code: null })]);
    expect(modeFor(refused ?? null)).toEqual({ kind: 'plain' });
  });
});

// Car 3c (8f4e9cc0 `follow_up_3c`): the drift rows come from the 05:20
// measurement and stay "adrift" until the next one, even after a
// publish answered exit 0 in between. The row is superseded when the
// kind's LATEST request wrote the tree's row after the measurement was
// read — and only then: a --check answers 0 without writing, a later
// refusal is the newer answer, an in-flight request is not an answer.
describe('rowState', () => {
  const measuredAt = '2026-09-15T05:20:00Z';
  const kind = adriftKinds([row({}), row({ field: 'label' })])[0]!;
  const published = 'publish-workflow: publishing infra/platform/workflows/ship-a-change.toml as automation:ops-runner for packet req-1 (boss: /usr/local/bin/boss)\n  published ship-a-change v32\npublish-workflow: ship-a-change v31 -> v32 live at http://10.20.0.34:7900 — confirmed by reading the active row back and comparing it to infra/platform/workflows/ship-a-change.toml (equal); packet req-1';
  const latest = (over: Parameters<typeof request>[0]): PublishRequest | null => parsePublishRequests([request(over)])[0] ?? null;

  test('no request, or one still open, leaves the row as the packet measured it', () => {
    expect(rowState(kind, null, measuredAt)).toEqual({ kind: 'adrift' });
    expect(rowState(kind, latest({ status: 'open', disposition: null }), measuredAt)).toEqual({ kind: 'adrift' });
  });

  test('exit 0 answered after the measurement supersedes the row, naming the versions and the instant', () => {
    expect(rowState(kind, latest({ output: published }), measuredAt)).toEqual({
      kind: 'superseded',
      request_id: 'req-1',
      at: '2026-09-15T21:03:12.123456Z',
      from: 31,
      to: 32,
    });
  });

  test('exit 0 answered BEFORE the measurement does not: the measurement already saw its effect', () => {
    expect(rowState(kind, latest({ output: published, opened_at: '2026-09-14T20:00:00+00:00', answered_at: '2026-09-14T20:01:00Z' }), measuredAt)).toEqual({ kind: 'adrift' });
  });

  test('a refusal or a non-zero exit after the measurement is the newer answer: not superseded', () => {
    expect(rowState(kind, latest({ exit_code: '78' }), measuredAt)).toEqual({ kind: 'adrift' });
    expect(rowState(kind, latest({ exit_code: '6' }), measuredAt)).toEqual({ kind: 'adrift' });
    expect(rowState(kind, latest({ disposition: 'refused', exit_code: null }), measuredAt)).toEqual({ kind: 'adrift' });
  });

  test('a --check that answers 0 wrote nothing, so it supersedes nothing', () => {
    expect(rowState(kind, latest({ args: ['ship-a-change', '--check'], output: 'publish-workflow: --check ok: would publish' }), measuredAt)).toEqual({ kind: 'adrift' });
  });

  test('versions come off the verb\'s confirmation line; unnamed, the measured live version is the from and the to is null', () => {
    const got = rowState(kind, latest({ output: 'published, somehow, without the line' }), measuredAt);
    expect(got).toMatchObject({ kind: 'superseded', from: 31, to: null });
  });

  test('an answer with no completed_at falls back to opened_at; unstamped both ways it cannot be judged and stays adrift', () => {
    expect(rowState(kind, latest({ answered_at: null, opened_at: '2026-09-15T21:00:00+00:00', output: published }), measuredAt)).toMatchObject({
      kind: 'superseded',
      at: '2026-09-15T21:00:00+00:00',
    });
    const unstamped = parsePublishRequests([{ ...request({ answered_at: null, output: published }), metadata: { host: 'boss-gcp', verb: PUBLISH_VERB, args: ['ship-a-change'] } }])[0] ?? null;
    expect(unstamped?.opened_at).toBeNull();
    expect(rowState(kind, unstamped, measuredAt)).toEqual({ kind: 'adrift' });
  });

  test('the header subtracts the fields of every superseded kind, never a kind that is still adrift', () => {
    const kinds = adriftKinds([row({}), row({ field: 'label' }), row({ kind: 'maintenance-sweep', live_version: 2 })]);
    const states = new Map([
      ['ship-a-change', rowState(kinds[0]!, latest({ output: published }), measuredAt)],
      ['maintenance-sweep', rowState(kinds[1]!, latest({ args: ['maintenance-sweep'], exit_code: '78' }), measuredAt)],
    ]);
    expect(supersededFieldCount(kinds, states)).toBe(2);
    expect(supersededFieldCount(kinds, new Map())).toBe(0);
  });
});

describe('forceConfirmed', () => {
  test('the second confirmation must name the field that will be erased, exactly', () => {
    expect(forceConfirmed('description', ['description'])).toBe(true);
    expect(forceConfirmed('  description ', ['description'])).toBe(true);
    expect(forceConfirmed('desc', ['description'])).toBe(false);
    expect(forceConfirmed('', ['description'])).toBe(false);
    expect(forceConfirmed('label, description', ['label', 'description'])).toBe(true);
    expect(forceConfirmed('description', ['label', 'description'])).toBe(false);
    expect(forceConfirmed('description', [])).toBe(false);
  });
});

describe('approveBody', () => {
  const kind = adriftKinds([row({}), row({ field: 'label' })])[0]!;
  const against = { packet: 'df43e97b-578b-40ad-a1c9-3c452e98dd01', head: '8c2eb523f515f6c8183d2d31a8b948db4a81fed5' };

  test('is the packet boss ops files: kind, title, subject custom/<host>, metadata host/verb/args', () => {
    const b = approveBody(kind, 'plain', against, 'emp-david');
    expect(b).toMatchObject({
      kind: 'ops-request',
      title: 'publish-workflow ship-a-change on boss-gcp',
      tags: [],
      subject: { id: PUBLISH_HOST, subject_kind: 'custom' },
      owner_id: 'emp-david',
      status: 'open',
      priority: 'standard',
    });
    expect(b.metadata).toMatchObject({
      host: 'boss-gcp',
      verb: 'publish-workflow',
      args: ['ship-a-change'],
      requested_from: 'drift-tab',
      drift_packet: against.packet,
      drift_head: against.head,
      drift_fields: ['description', 'label'],
      live_version: 31,
    });
    // No opened_on: the server clocks the packet and stamps
    // metadata.opened_at, the instant the probe reads.
    expect('opened_on' in b).toBe(false);
  });

  test('the force mode carries --force-tree as the second arg and the title says so', () => {
    const b = approveBody(kind, 'force', against, 'emp-david');
    expect(b.metadata.args).toEqual(['ship-a-change', '--force-tree']);
    expect(b.title).toBe('publish-workflow ship-a-change --force-tree on boss-gcp');
    expect(b.metadata.force_erases).toEqual(['description', 'label']);
  });

  test('a null head is carried as null, never as the string "null"', () => {
    const b = approveBody(kind, 'plain', { packet: 'p', head: null }, 'emp-david');
    expect(b.metadata.drift_head).toBeNull();
  });
});

describe('authority', () => {
  test('the role is read off the ops-request row: the execute step names it', () => {
    const wf = { kind: 'ops-request', steps: [{ title: 'filed', authority_role: null }, { title: 'execute', authority_role: 'platform-admin' }] };
    expect(executeAuthorityRole(wf)).toBe('platform-admin');
    expect(executeAuthorityRole({ steps: [{ title: 'execute' }] })).toBeNull();
    expect(executeAuthorityRole(null)).toBeNull();
  });

  test('the viewer is admitted by that role, or refused with the role named', () => {
    expect(approveAuthority('platform-admin', 'platform-admin')).toEqual({ kind: 'admitted' });
    expect(approveAuthority(null, 'brewer')).toEqual({ kind: 'admitted' });
    expect(approveAuthority('platform-admin', 'brewer')).toMatchObject({ kind: 'refused', role: 'platform-admin' });
    expect(approveAuthority('platform-admin', null)).toMatchObject({ kind: 'refused' });
  });
});
