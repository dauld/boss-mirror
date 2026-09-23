import { describe, expect, test } from 'bun:test';
import { relatedJobsUrl } from './relatedJobs';

// The KB page asked `/api/jobs?account_id=…` (or `asset_id=…`) for an
// entity's related jobs. The listing has one subject filter, spelled
// `subject_id` for every kind, and it ignored the alias — so every KB
// page's "related jobs" was the first fifty jobs of the whole company.
// Since 2026-09-23 the listing refuses an unknown parameter with a 400
// (backlog 7f3e871a), which would have turned that silent wrong answer
// into an error panel; this pins the spelling it actually reads.
describe('relatedJobsUrl', () => {
  test('filters on subject_id, the one subject filter the listing reads', () => {
    const url = new URL(relatedJobsUrl('acct 1/x'), 'http://x');
    expect(url.pathname).toBe('/api/jobs');
    expect(url.searchParams.get('subject_id')).toBe('acct 1/x');
    expect(url.searchParams.get('limit')).toBe('50');
  });

  test('sends no per-kind alias the listing would refuse', () => {
    const keys = [...new URL(relatedJobsUrl('a-1'), 'http://x').searchParams.keys()];
    expect(keys.sort()).toEqual(['limit', 'subject_id']);
  });
});
