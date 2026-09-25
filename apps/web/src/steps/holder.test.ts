// An active step keeps the holder that claimed it (backlogs 650ebd0c,
// 0f42efa0). The jobs API refuses a PUT that replaces the holder of an
// ACTIVE step, or clears it without releasing the step, so a surface
// that offers the assignee picker on such a step offers a write the
// server refuses — and before the refusal existed, that picker was how
// an active step silently changed hands.

import { describe, expect, test } from 'bun:test';
import { assigneeToSend, holderLocked } from './holder';

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

describe('assigneeToSend', () => {
  test('a locked step sends its stored holder, whatever the picker holds', () => {
    // The picker can be stale — carried over from the previous step the
    // reused surface showed (backlog 848477c3) — and a Complete that
    // sent it met the 409, or before that, reassigned in silence.
    const held = { status: 'active', assignee_id: 'emp-claimant' } as const;
    expect(assigneeToSend(held, 'emp-stale')).toBe('emp-claimant');
    expect(assigneeToSend(held, '')).toBe('emp-claimant');
  });

  test('otherwise the picker is the answer, and empty means nobody', () => {
    expect(assigneeToSend({ status: 'ready', assignee_id: 'emp-a' }, 'emp-b')).toBe('emp-b');
    expect(assigneeToSend({ status: 'ready', assignee_id: 'emp-a' }, '')).toBe(null);
    expect(assigneeToSend({ status: 'active', assignee_id: null }, 'emp-b')).toBe('emp-b');
  });
});
