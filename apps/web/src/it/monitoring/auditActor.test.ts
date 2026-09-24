import { describe, expect, test } from 'bun:test';
import { actorOf, knownActors } from './auditActor';

describe('actorOf — who acted, read off an audit row (backlog 03f79eca)', () => {
  test('reads the _actor stamp every writer puts on its payload', () => {
    expect(actorOf({ id: 'job-1', _actor: 'agent-claude' })).toBe('agent-claude');
  });

  test('a row predating the stamp, or a payload that is not an object, has no actor', () => {
    expect(actorOf({ id: 'job-1' })).toBeNull();
    expect(actorOf(null)).toBeNull();
    expect(actorOf('text')).toBeNull();
    expect(actorOf([{ _actor: 'x' }])).toBeNull();
  });

  test('a blank or non-string stamp is not an actor', () => {
    expect(actorOf({ _actor: '' })).toBeNull();
    expect(actorOf({ _actor: 42 })).toBeNull();
  });
});

describe('knownActors — the filter datalist, from the rows on screen', () => {
  test('distinct, sorted, rows without an actor skipped', () => {
    const rows = [
      { payload: { _actor: 'emp-david' } },
      { payload: { _actor: 'agent-claude' } },
      { payload: {} },
      { payload: { _actor: 'emp-david' } },
    ];
    expect(knownActors(rows)).toEqual(['agent-claude', 'emp-david']);
  });
});
