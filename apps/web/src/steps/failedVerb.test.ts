import { describe, expect, test } from 'bun:test';
import { failedVerb, failedVerbPhrase } from './failedVerb';

// The exact shape jobs.complete_linked_step (v6, backlog f47861a5)
// writes onto a step whose verb FAILED — pinned by
// boss-dispatcher-handlers/tests/publish_pr_answer.rs on the other end.
const LINE =
  'publish-github-pr: FAILED — pushing publish/2026-09-18 to the forge (ssh://…) as david: fatal: detected dubious ownership';
const ANNOTATED = {
  ops_verb: 'publish-github-pr',
  failed: LINE,
  failed_exit: '1',
  failed_source: 'c98a782f-0000-4000-8000-000000000000',
  alert: 'a1e57000-0000-4000-8000-000000000000',
};

describe('a failed verb answer on a step', () => {
  test('is read off the four keys the annotating handler writes', () => {
    expect(failedVerb(ANNOTATED)).toEqual({
      line: LINE,
      exit: '1',
      source: 'c98a782f-0000-4000-8000-000000000000',
      alert: 'a1e57000-0000-4000-8000-000000000000',
    });
  });
  test('the line alone is a failure; the rest is optional and read as null', () => {
    expect(failedVerb({ failed: LINE })).toEqual({ line: LINE, exit: null, source: null, alert: null });
    // A numeric exit (an older writer, or a hand annotation) reads as its digits.
    expect(failedVerb({ failed: LINE, failed_exit: 1 })?.exit).toBe('1');
  });
  test('no line, an empty line, or no metadata at all is not a failure', () => {
    expect(failedVerb({ ops_verb: 'publish-github-pr' })).toBeNull();
    expect(failedVerb({ failed: '' })).toBeNull();
    expect(failedVerb({ failed: 42 })).toBeNull();
    expect(failedVerb(null)).toBeNull();
    expect(failedVerb(undefined)).toBeNull();
  });
  test('the phrase names the exit when one was recorded and the line always', () => {
    expect(failedVerbPhrase({ line: LINE, exit: '1', source: null, alert: null })).toBe(
      `FAILED (exit 1) · ${LINE}`,
    );
    expect(failedVerbPhrase({ line: LINE, exit: null, source: null, alert: null })).toBe(
      `FAILED · ${LINE}`,
    );
  });
});
