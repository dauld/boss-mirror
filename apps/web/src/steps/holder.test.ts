// An active step keeps the holder that claimed it (backlogs 650ebd0c,
// 0f42efa0). The jobs API refuses a PUT that replaces the holder of an
// ACTIVE step, or clears it without releasing the step, so a surface
// that offers the assignee picker on such a step offers a write the
// server refuses — and before the refusal existed, that picker was how
// an active step silently changed hands.

import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import {
  AGENT_RUN_EDGE,
  RELEASE,
  RELEASED_KEY,
  assigneeChange,
  gestureFields,
  holderLocked,
  releaseMetadata,
  releaseUnconfirmed,
} from './holder';

describe('holderLocked', () => {
  test('an active step with a holder is locked', () => {
    expect(holderLocked({ status: 'active', assignee_id: 'emp-claimant' })).toBe(true);
  });

  test('an active step nobody holds has nothing to keep', () => {
    expect(holderLocked({ status: 'active', assignee_id: null })).toBe(false);
    expect(holderLocked({ status: 'active', assignee_id: '' })).toBe(false);
    expect(holderLocked({ status: 'active', assignee_id: '  ' })).toBe(false);
  });

  test('a ready nomination can still move, and a finished step is the freeze’s', () => {
    expect(holderLocked({ status: 'ready', assignee_id: 'emp-pick' })).toBe(false);
    expect(holderLocked({ status: 'pending', assignee_id: 'emp-pick' })).toBe(false);
    expect(holderLocked({ status: 'completed', assignee_id: 'emp-pick' })).toBe(false);
  });
});

describe('assigneeChange', () => {
  test('a locked step sends no holder at all, whatever the picker holds', () => {
    // The picker can be stale — carried over from the previous step the
    // reused surface showed (backlog 848477c3). Omitting the key keeps
    // the stored holder, which is all a write to a held step may do.
    const held = { status: 'active', assignee_id: 'emp-claimant' } as const;
    expect(assigneeChange(held, 'emp-stale')).toEqual({});
    expect(assigneeChange(held, '')).toEqual({});
  });

  test('an untouched picker sends nothing — the snapshot is not a change', () => {
    // Backlog 6ef4a36b: a surface read while the step was ready and
    // unassigned sent `assignee_id: null` from that snapshot on every
    // Save, so a Save after an agent's claim tried to clear its holder.
    expect(assigneeChange({ status: 'ready', assignee_id: null }, '')).toEqual({});
    expect(assigneeChange({ status: 'ready', assignee_id: 'emp-a' }, 'emp-a')).toEqual({});
  });

  test('a pick the operator made is sent, and empty means nobody', () => {
    expect(assigneeChange({ status: 'ready', assignee_id: 'emp-a' }, 'emp-b')).toEqual({
      assignee_id: 'emp-b',
    });
    expect(assigneeChange({ status: 'ready', assignee_id: 'emp-a' }, '')).toEqual({
      assignee_id: null,
    });
    expect(assigneeChange({ status: 'active', assignee_id: null }, 'emp-b')).toEqual({
      assignee_id: 'emp-b',
    });
  });
});

describe('gestureFields — status only when the gesture changes it', () => {
  test('a Save sends no status, so a stale snapshot cannot release a live claim', () => {
    // THE DEFECT (backlog 6ef4a36b, the review of car 781b9209): the
    // surfaces sent `status: overrides.status ?? step.status`. A surface
    // read while the step was ready, saved after an agent claimed it,
    // sent `{status: ready, assignee_id: null}` — the release body — and
    // the server, rightly, released the claim.
    const staleSnapshot = { status: 'ready', assignee_id: null } as const;
    expect(gestureFields(staleSnapshot, '')).toEqual({});
  });

  test('a Start or a Complete sends the status it moves to', () => {
    const s = { status: 'active', assignee_id: 'emp-a' } as const;
    expect(gestureFields(s, 'emp-a', 'completed')).toEqual({ status: 'completed' });
    expect(gestureFields({ status: 'pending', assignee_id: null }, 'emp-b', 'active')).toEqual({
      status: 'active',
      assignee_id: 'emp-b',
    });
  });
});

describe('the release', () => {
  test('is the one body that takes an active step off its holder', () => {
    expect(RELEASE).toEqual({ status: 'ready', assignee_id: null });
  });

  const AT = '2026-09-25T16:00:00.000Z';

  test('clears the run edge ALWAYS, whatever the page was drawn with', () => {
    // A step completed after a release delivers onto whatever run its
    // edge names (b91a2103). The page's copy of the metadata is a
    // snapshot: drawn before the dispatcher wrote `agent_run`, it held
    // none, and a release that cleared the edge only when the snapshot
    // had one left a Ready step naming a live run (the review of car
    // 675f1858, #1). `boss step release` sends the null unconditionally
    // and the merge door deletes a missing key quietly, so the page does
    // the same.
    expect(releaseMetadata({ brief: 'kept' }, 'handing over', 'emp-001', AT)).toMatchObject({
      [AGENT_RUN_EDGE]: null,
    });
    expect(releaseMetadata({}, 'handing over', 'emp-001', AT)).toMatchObject({
      [AGENT_RUN_EDGE]: null,
    });
  });

  test('records the same released stamp `boss step release` records', () => {
    // The review of car 675f1858, #2: the page's release left no reason
    // and no stamp, so a step it freed read to the next operator exactly
    // like the stall it was. The shape is release_patch's in
    // boss-cli/src/steps.rs: {why, by, at, from_run}.
    expect(
      releaseMetadata({ [AGENT_RUN_EDGE]: 'run-1', brief: 'kept' }, '  handing over  ', 'emp-001', AT),
    ).toEqual({
      [AGENT_RUN_EDGE]: null,
      [RELEASED_KEY]: { why: 'handing over', by: 'emp-001', at: AT, from_run: 'run-1' },
    });
    expect(releaseMetadata({}, 'why', null, AT)).toEqual({
      [AGENT_RUN_EDGE]: null,
      [RELEASED_KEY]: { why: 'why', by: null, at: AT, from_run: null },
    });
  });

  test('names the stamp the CLI names (boss-cli steps::RELEASED_KEY)', () => {
    // A fact that lives twice gets an equality test (CLAUDE.md §9a).
    const src = readFileSync(
      join(
        import.meta.dir, '..', '..', '..', '..',
        'crates', 'orchestrators', 'boss-cli', 'src', 'steps.rs',
      ),
      'utf8',
    );
    const key = src.match(/pub\(crate\) const RELEASED_KEY: &str = "([^"]+)";/);
    expect(key, 'boss-cli steps::RELEASED_KEY is where the CLI spells it').not.toBeNull();
    expect(RELEASED_KEY).toBe(key![1]!);
  });

  test('names the edge the server names (boss_jobs::agent_runs::EDGE_KEY)', () => {
    // A fact that lives twice gets an equality test (CLAUDE.md §9a).
    const src = readFileSync(
      join(
        import.meta.dir, '..', '..', '..', '..',
        'crates', 'core', 'boss-jobs', 'src', 'agent_runs', 'mod.rs',
      ),
      'utf8',
    );
    const key = src.match(/pub const EDGE_KEY: &str = "([^"]+)";/);
    expect(key, 'boss_jobs::agent_runs::EDGE_KEY is where the server spells it').not.toBeNull();
    expect(AGENT_RUN_EDGE).toBe(key![1]!);
  });
});

describe('releaseUnconfirmed — the read-back is the fact (boss-cli confirm_released)', () => {
  const released = (over: Record<string, unknown> = {}) => ({
    status: 'ready',
    assignee_id: null,
    metadata: { [RELEASED_KEY]: { why: 'handing over' } },
    ...over,
  });

  test('a ready, nobody-held step carrying the reason and no run is released', () => {
    expect(releaseUnconfirmed(released(), ' handing over ')).toBeNull();
  });

  test('each way a release half-happens is named', () => {
    expect(releaseUnconfirmed(released({ status: 'active' }), 'handing over')).toMatch(
      /reads back active/,
    );
    expect(
      releaseUnconfirmed(released({ assignee_id: 'agent-claude' }), 'handing over'),
    ).toMatch(/still assigned to agent-claude/);
    expect(
      releaseUnconfirmed(
        released({
          metadata: { [AGENT_RUN_EDGE]: 'run-1', [RELEASED_KEY]: { why: 'handing over' } },
        }),
        'handing over',
      ),
    ).toMatch(/still names agent_run run-1/);
    expect(releaseUnconfirmed(released({ metadata: {} }), 'handing over')).toMatch(
      /reason/,
    );
  });
});
