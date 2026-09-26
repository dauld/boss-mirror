// A body the job page cannot render is a FAILED read, named (backlog
// c2e18fdd, 2026-09-26). JobDetailPage cast any 2xx body to Job, and the
// mocked api floor answers an unrouted `/api/jobs/<id>` with `[]` —
// truthy, so the page rendered it, and the Subject section threw
// "Cannot read properties of undefined (reading 'id')" in subjectPath
// under a passing inbox spec. The parse checks exactly what the page
// dereferences: an object with an id, a subject object, and steps that
// are a list when present. Every other field renders as it comes.
import { describe, expect, it } from 'bun:test';
import { parseJob } from './types';

const READ = '/api/jobs/job-2';

const JOB = {
  id: 'job-2', kind: 'user-feedback', title: 'A packet', status: 'open',
  subject: { subject_kind: 'custom', id: '/inbox' },
  owner_id: 'emp-david', priority: 'standard', opened_on: '2026-09-26',
  due_on: null, closed_on: null, metadata: {}, tags: [], steps: [],
};

describe('parseJob', () => {
  it('returns a whole Job as it came', () => {
    expect(parseJob(READ, JOB)).toEqual(JOB as never);
  });

  it('accepts a Job with no steps key — the list is optional on the wire', () => {
    const { steps: _steps, ...noSteps } = JOB;
    expect(parseJob(READ, noSteps).id).toBe('job-2');
  });

  for (const [what, body] of [
    ['the mocked floor’s []', []],
    ['null', null],
    ['a string', 'job-2'],
    ['an object with no id', { ...JOB, id: undefined }],
  ] as const) {
    it(`refuses ${what} as not a Job, naming the read`, () => {
      expect(() => parseJob(READ, body)).toThrow(`${READ}: the answer is not a Job`);
    });
  }

  it('refuses a Job with no subject — the shape that threw in subjectPath', () => {
    const { subject: _subject, ...noSubject } = JOB;
    expect(() => parseJob(READ, noSubject)).toThrow(`${READ}: the Job carries no subject`);
    expect(() => parseJob(READ, { ...JOB, subject: null })).toThrow(
      `${READ}: the Job carries no subject`,
    );
  });

  it('refuses steps that are not a list', () => {
    expect(() => parseJob(READ, { ...JOB, steps: {} })).toThrow(
      `${READ}: the Job's steps are not a list`,
    );
  });
});
