// The position key must match the server's fleet grouping —
// COALESCE(NULLIF(spec_slug,''), title) — or item lists sit under
// the wrong node badge.
import { describe, expect, it } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { groupByPosition, positionOf, standingAt, standingNote, standingOf } from './position';
import type { Job } from './types';

function job(id: string, priority: string, opened: string, steps: unknown[]): Job {
  return { id, priority, opened_on: opened, steps } as unknown as Job;
}

describe('positionOf', () => {
  it('uses spec_slug of the first in-flight step', () => {
    const j = job('a', 'standard', '2026-08-01', [
      { status: 'completed', title: 'Old', spec_slug: 'old' },
      { status: 'ready', title: 'Triage feedback', spec_slug: 'triage' },
      { status: 'pending', title: 'Later', spec_slug: 'later' },
    ]);
    expect(positionOf(j)).toBe('triage');
  });

  it('falls back to the title when the slug is empty or absent — the pre-migration shape', () => {
    const empty = job('a', 'standard', '2026-08-01', [
      { status: 'active', title: 'Deliver', spec_slug: '' },
    ]);
    const absent = job('b', 'standard', '2026-08-01', [{ status: 'ready', title: 'Deliver' }]);
    expect(positionOf(empty)).toBe('Deliver');
    expect(positionOf(absent)).toBe('Deliver');
  });

  it('is null for a Job with nothing in flight', () => {
    expect(positionOf(job('a', 'standard', '2026-08-01', [{ status: 'completed', title: 'X' }]))).toBeNull();
  });
});

describe('groupByPosition', () => {
  it('orders each queue priority-first then oldest-first', () => {
    const grouped = groupByPosition([
      job('old-standard', 'standard', '2026-08-01', [{ status: 'ready', spec_slug: 't', title: 'T' }]),
      job('new-urgent', 'urgent', '2026-08-08', [{ status: 'ready', spec_slug: 't', title: 'T' }]),
      job('new-standard', 'standard', '2026-08-08', [{ status: 'ready', spec_slug: 't', title: 'T' }]),
    ]);
    expect(grouped.get('t')!.map((j) => j.id)).toEqual([
      'new-urgent',
      'old-standard',
      'new-standard',
    ]);
  });
});

describe('standingAt — the phrase for a step a packet is standing at', () => {
  it('names the status beside the title, because a perfect-tense title alone reads as done', () => {
    expect(standingAt('Report recorded', 'ready')).toBe('Report recorded (ready, not yet done)');
    expect(standingNote('active')).toBe('active, not yet done');
    expect(standingAt('Built', 'active')).toBe(`Built (${standingNote('active')})`);
  });

  it('equals boss_jobs::yard::standing_at (crates/core/boss-jobs/src/yard.rs)', () => {
    // A fact that lives twice gets an equality test (CLAUDE.md §9a).
    // The collapse — every surface showing the server's phrase as-is —
    // would need a derived field on every Step the jobs API serializes;
    // until one exists the web spells it here, held to the server's
    // format string (3102fe7a).
    const src = readFileSync(
      join(import.meta.dir, '..', '..', '..', '..', 'crates', 'core', 'boss-jobs', 'src', 'yard.rs'),
      'utf8',
    );
    const fn = src.match(
      /pub fn standing_at\(title: &str, status: &str\) -> String \{\s*format!\("([^"]*)"\)\s*\}/,
    );
    expect(fn, 'boss_jobs::yard::standing_at is where the server spells the phrase').not.toBeNull();
    const server = (title: string, status: string): string =>
      fn![1]!.replace('{title}', title).replace('{status}', status);
    for (const [title, status] of [
      ['DEPARTED — merged into main', 'ready'],
      ['In transit — cluster converged', 'active'],
    ] as const) {
      expect(standingAt(title, status)).toBe(server(title, status));
    }
  });
});

describe('standingOf — where a Job stands, as the standing phrase', () => {
  it('names the position with the current step\'s status beside it', () => {
    const j = job('a', 'standard', '2026-08-01', [
      { status: 'completed', title: 'Opened', spec_slug: 'opened' },
      { status: 'ready', title: 'Promoted: backlog-item', spec_slug: 'promoted' },
    ]);
    expect(standingOf(j)).toBe('promoted (ready, not yet done)');
  });

  it('is null when no step is in flight — nothing is stood at', () => {
    const j = job('a', 'standard', '2026-08-01', [
      { status: 'completed', title: 'Done', spec_slug: 'done' },
    ]);
    expect(standingOf(j)).toBeNull();
  });
});
