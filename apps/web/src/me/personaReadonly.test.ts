// The guest→operator persona transition must END the guest.
//
// `session.readonly` is set when the gateway probe classifies a guest
// (classifyProbe), and MePage forks on it: readonly renders GuestHome,
// an operator renders the My Day board. `setPersona` swapped
// `session.value` to the chosen employee but left `readonly` standing
// — so a tab that had ever been a guest kept rendering GuestHome (and
// every WriteGate stayed disabled) even after the session became a
// real operator. Once a guest, GuestHome forever.
//
// Tested here, at the consuming layer: this file runs under the
// app's unit gate, where the transition MePage actually forks on is
// asserted as a value. The rendering half (disabled → live controls)
// is the mocked suite's job (readonly-session.mocked.spec.ts).

import { beforeEach, describe, expect, test } from 'bun:test';
import {
  guestEmployee,
  session,
  setPersona,
  type Employee,
} from '@boss/web-kit/session/session.svelte';

const OPERATOR: Employee = {
  id: 'emp-001', name: 'Demo CEO', email: 'ceo@demo', role: 'ceo',
  department: 'exec', hire_date: '2020-01-01', status: 'active',
  location: 'HQ', employment_type: 'full-time', skills: [], certifications: [],
};

// The persona is read as its own people row (backlog b4f68a65): the
// roster the shell used to preload is gone, so the test answers the
// one read setPersona makes.
const people = async (url: string) => {
  const found = url === `/api/people/${OPERATOR.id}`;
  return { ok: found, status: found ? 200 : 404, json: async () => OPERATOR };
};

function becomeGuest(): void {
  session.fromGateway = true;
  session.readonly = true;
  session.value = { kind: 'ready', user: guestEmployee('guest@algedonic.dev') };
}

describe('setPersona ends the guest identity', () => {
  beforeEach(becomeGuest);

  test('switching guest → rostered operator clears readonly', async () => {
    await setPersona(OPERATOR.id, people);

    expect(session.value.kind).toBe('ready');
    if (session.value.kind === 'ready') {
      expect(session.value.user.id).toBe(OPERATOR.id);
    }
    // The defect: this stayed `true`, so the operator kept the guest's
    // render — GuestHome instead of My Day, every write surface inert.
    expect(session.readonly).toBe(false);
    expect(session.fromGateway).toBe(false);
  });

  test('an id the people service does not hold changes nothing', async () => {
    await setPersona('emp-nobody', people);

    // No identity change, no state change — the guest stays a guest
    // rather than becoming a half-cleared chimera.
    expect(session.readonly).toBe(true);
    expect(session.value.kind).toBe('ready');
    if (session.value.kind === 'ready') {
      expect(session.value.user.name).toBe('Guest');
    }
  });
});
