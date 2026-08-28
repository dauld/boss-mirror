// The resolution chain for the decision-context panel (19db52de):
// step's own context_md wins, then the job's, then the job's filed
// message — and blank strings are absences, not content.

import { describe, expect, test } from 'bun:test';
import {
  contextFromJob,
  contextFromPriorSteps,
  contextFromStep,
} from './decisionContext';

describe('decision context resolution', () => {
  test('a step that carries its own context wins outright', () => {
    expect(contextFromStep({ context_md: 'approve the 62-commit push' })).toEqual({
      text: 'approve the 62-commit push',
      source: 'step',
    });
  });

  test('a blank or missing step context is an absence', () => {
    expect(contextFromStep({})).toBeNull();
    expect(contextFromStep({ context_md: '   ' })).toBeNull();
    expect(contextFromStep({ context_md: 42 })).toBeNull();
  });

  test('the job falls back context_md then message', () => {
    expect(
      contextFromJob({ context_md: 'briefing', message: 'filed text' }),
    ).toEqual({ text: 'briefing', source: 'job-context' });
    expect(contextFromJob({ message: 'filed text' })).toEqual({
      text: 'filed text',
      source: 'job-message',
    });
    expect(contextFromJob({})).toBeNull();
  });

  // The bug David hit on 2026-08-28: a backlog-item states its case in
  // `body`, a user-feedback packet in `message`. Covering one and not
  // the other left every backlog-item decision with an empty panel.
  test('a backlog-item states its case in body', () => {
    expect(contextFromJob({ body: 'the jobs API back door is unauthenticated' })).toEqual({
      text: 'the jobs API back door is unauthenticated',
      source: 'job-body',
    });
    // message still wins when both are present: it is the filed text.
    expect(contextFromJob({ message: 'filed', body: 'later' })).toEqual({
      text: 'filed',
      source: 'job-message',
    });
  });

  test('every user-feedback packet is self-presenting via its message', () => {
    // The 28 Decide-the-design steps in David's queue carry no
    // context_md anywhere; their case is the filed message. The chain
    // must surface it rather than render another empty form.
    const jobMeta = { message: 'My Day cannot say whether a packet needs me' };
    expect(contextFromJob(jobMeta)?.source).toBe('job-message');
  });
});

describe('the fourth source: what earlier steps recorded', () => {
  // The case that reached David with an empty panel on 2026-08-28: a
  // protocol-retro whose findings live in four completed steps, and a
  // rotate-a-credential whose case lives in `scope`. Neither carries
  // context_md or message anywhere.
  const longA = 'A'.repeat(200);
  const longB = 'B'.repeat(200);

  test('prose from completed steps is gathered, labelled by step', () => {
    const got = contextFromPriorSteps([
      { title: 'Assess bottlenecks', status: 'completed', metadata: { bottleneck: longA } },
      { title: 'Write the retro report', status: 'completed', metadata: { report: longB } },
    ]);
    expect(got?.source).toBe('prior-steps');
    expect(got?.text).toContain('### Assess bottlenecks');
    expect(got?.text).toContain('**bottleneck**');
    expect(got?.text).toContain(longA);
    expect(got?.text).toContain('### Write the retro report');
    expect(got?.text).toContain(longB);
  });

  test('a step that is not done is not yet a case', () => {
    expect(
      contextFromPriorSteps([{ title: 'Ready', status: 'ready', metadata: { note: longA } }]),
    ).toBeNull();
  });

  // A receipt, a sha or a disposition is a field. Pasting them under a
  // decision buries the prose that matters.
  test('short values and known non-prose keys are skipped', () => {
    expect(
      contextFromPriorSteps([
        {
          title: 'Green, and observed working',
          status: 'completed',
          metadata: { gates: 'full', receipt: longA, branch: longB, verified: 'ok' },
        },
      ]),
    ).toBeNull();
  });

  test('nothing to gather is an absence, not an empty panel', () => {
    expect(contextFromPriorSteps([])).toBeNull();
    expect(
      contextFromPriorSteps([{ title: 'x', status: 'completed', metadata: null }]),
    ).toBeNull();
  });
});
