import { describe, expect, test } from 'bun:test';
import { jobsFilterSearch } from './filterQuery';

// /ux/jobs's filters changed page state and never the URL, so a reload
// dropped the filter and a filtered view could not be shared (backlog
// f8027805). The page now writes back the query parseRoute already
// reads; this is the write, pinned against that read's own rules.
describe('jobsFilterSearch', () => {
  const none = { kind: '', status: 'open', subjectId: '' };

  test('an unchanged filter leaves the URL exactly as it was', () => {
    // Mount runs the write too: it must not normalise a deep link.
    expect(jobsFilterSearch('', none)).toBe('');
    expect(jobsFilterSearch('?status=open&owner_id=emp-1', none)).toBe('?status=open&owner_id=emp-1');
    expect(jobsFilterSearch('?status=&owner_id=emp-1', { ...none, status: '' })).toBe(
      '?status=&owner_id=emp-1',
    );
  });

  test('All is written as an empty status, which the router reads as every status', () => {
    expect(jobsFilterSearch('', { ...none, status: '' })).toBe('?status=');
  });

  test('a chosen status is written, and choosing the default again takes it back out', () => {
    expect(jobsFilterSearch('', { ...none, status: 'closed' })).toBe('?status=closed');
    // An ABSENT status reads as open, so open needs no parameter.
    expect(jobsFilterSearch('?status=closed', none)).toBe('');
  });

  test('kind and subject id are written when set and removed when cleared', () => {
    expect(jobsFilterSearch('', { kind: 'sale', status: 'open', subjectId: 'acc-1' })).toBe(
      '?kind=sale&subject_id=acc-1',
    );
    expect(jobsFilterSearch('?kind=sale&subject_id=acc-1', none)).toBe('');
  });

  test('parameters the filters do not own ride through untouched', () => {
    expect(
      jobsFilterSearch('?owner_id=emp-1&kind_prefix=ship&new=1', { ...none, status: 'blocked' }),
    ).toBe('?owner_id=emp-1&kind_prefix=ship&new=1&status=blocked');
  });
});
