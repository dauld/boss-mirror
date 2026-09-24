import { describe, expect, it } from 'bun:test';
import { ageText, parseRuleFirings, ruleActivity, type RuleFirings } from './ruleFirings';
import type { DispatcherRule } from './types';

// The rules list's two activity cells (backlog 43c4451a, page-audit
// 08a444bc gap 5). The case the packet was filed on is the first
// describe: a stalled rule and an idle one must never print the same
// row. The second is the rule under every surface here — a record the
// server could not read prints "unknown", never "none".

const NOW = '2026-09-24T12:00:00Z';

const rule = (over: Partial<DispatcherRule> = {}): DispatcherRule => ({
  name: 'auto-park-on-gate-green',
  on_event: 'step.done.gate-verdict',
  when: null,
  do: [{ handler: 'jobs.auto-park', args: {} }],
  version: 2,
  ...over,
});

const read = (over: Partial<RuleFirings> = {}) => ({
  kind: 'ready' as const,
  data: {
    now: NOW,
    retention_days: 30,
    firings: [{ rule: 'auto-park-on-gate-green', fired_on: 'step.done.gate-verdict', fired_at: '2026-09-24T11:00:00Z' }],
    firings_error: null,
    dead_letters: [],
    dead_letters_error: null,
    ...over,
  },
});

describe('stalled and idle are two different rows', () => {
  it('an idle rule: fired an hour ago, nothing dead-lettered', () => {
    const a = ruleActivity(rule(), read());
    expect(a.lastFired).toBe('1h ago');
    expect(a.lastFiredWhy).toBe('fired 2026-09-24T11:00:00Z on step.done.gate-verdict');
    expect(a.deadLetters).toBe('none');
    expect(a.failing).toBe(false);
  });

  it('a stalled rule: its newest dead-letter is later than its newest firing', () => {
    const a = ruleActivity(
      rule(),
      read({
        dead_letters: [{ rule: 'auto-park-on-gate-green', packets: 2, newest_at: '2026-09-24T11:45:00Z', newest_job_id: 'j-2' }],
      }),
    );
    expect(a.lastFired).toBe('1h ago');
    expect(a.deadLetters).toBe('2, newest 15m ago — failing since its last firing');
    expect(a.deadLetterJob).toBe('j-2');
    expect(a.failing).toBe(true);
  });

  it('a rule that failed and then fired again is not failing now', () => {
    const a = ruleActivity(
      rule(),
      read({
        dead_letters: [{ rule: 'auto-park-on-gate-green', packets: 1, newest_at: '2026-09-22T11:00:00Z', newest_job_id: 'j-1' }],
      }),
    );
    expect(a.deadLetters).toBe('1, newest 2d ago');
    expect(a.failing).toBe(false);
  });

  it('a rule that only dead-letters, with no firing in the window, is failing', () => {
    const a = ruleActivity(
      rule(),
      read({
        firings: [],
        dead_letters: [{ rule: 'auto-park-on-gate-green', packets: 1, newest_at: '2026-09-24T10:00:00Z', newest_job_id: 'j-1' }],
      }),
    );
    expect(a.lastFired).toBe('none in 30d');
    expect(a.deadLetters).toBe('1, newest 2h ago — failing, no firing recorded');
    expect(a.failing).toBe(true);
  });
});

describe('unread is never none', () => {
  it('a failed read prints unknown in both cells, with the reason', () => {
    const a = ruleActivity(rule(), { kind: 'failed', error: '/api/yard/rule-firings: HTTP 503' });
    expect([a.lastFired, a.deadLetters]).toEqual(['unknown', 'unknown']);
    expect(a.lastFiredWhy).toContain('HTTP 503');
    expect(a.failing).toBe(false);
  });

  it('each half the server could not read prints unknown on its own', () => {
    const a = ruleActivity(rule(), read({ firings: null, firings_error: 'not wired', dead_letters: null, dead_letters_error: 'no scope' }));
    expect([a.lastFired, a.lastFiredWhy, a.deadLetters, a.deadLettersWhy]).toEqual(['unknown', 'not wired', 'unknown', 'no scope']);
  });

  it('a scheduled rule is not recorded, not silent', () => {
    const a = ruleActivity(rule({ name: 'retro', on_event: undefined, schedule: { cadence: 'weekly', anchor_date: '2026-09-21' } }), read());
    expect(a.lastFired).toBe('not recorded');
    expect(a.lastFiredWhy).toContain('scheduled');
  });

  it('while the read is in flight nothing claims a state', () => {
    const a = ruleActivity(rule(), { kind: 'loading' });
    expect([a.lastFired, a.deadLetters, a.failing]).toEqual(['…', '…', false]);
  });
});

describe('the payload', () => {
  it('parses both halves and keeps a null half null with its reason', () => {
    const p = parseRuleFirings({
      now: NOW, retention_days: 30,
      firings: null, firings_error: 'reading dispatcher_firings: boom',
      dead_letters: [{ rule: 'r', packets: 3, newest_at: null, newest_job_id: null }], dead_letters_error: null,
    });
    expect(p.firings).toBeNull();
    expect(p.firings_error).toBe('reading dispatcher_firings: boom');
    expect(p.dead_letters).toEqual([{ rule: 'r', packets: 3, newest_at: null, newest_job_id: null }]);
  });

  it('refuses a body that is not the record — the crawl fallthrough is not "no firings"', () => {
    expect(() => parseRuleFirings([])).toThrow();
    expect(() => parseRuleFirings({})).toThrow();
  });

  it('ages in minutes, hours, then days, and never invents one', () => {
    expect(ageText('2026-09-24T11:59:00Z', NOW)).toBe('1m');
    expect(ageText('2026-09-23T12:00:00Z', NOW)).toBe('24h');
    expect(ageText('2026-09-20T12:00:00Z', NOW)).toBe('4d');
    expect(ageText('not a time', NOW)).toBe('');
  });
});
