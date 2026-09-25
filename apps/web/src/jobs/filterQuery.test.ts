import { describe, expect, test } from 'bun:test';
import { jobsFilterSearch, searchWithoutNewJob } from './filterQuery';

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
      jobsFilterSearch('?owner_id=emp-1&kind_prefix=ship&new=1', { ...none, status: 'closed' }),
    ).toBe('?owner_id=emp-1&kind_prefix=ship&new=1&status=closed');
  });

  // Under `new=1` the router reads subject_id as the new job's subject,
  // not as the filter (backlog d0b93b80). The write follows that read:
  // a mount must not strip the deep link's subject, and a filter typed
  // while the form is open waits for Cancel rather than overwrite it.
  test('under new=1, subject_id is the new job subject and the write leaves it alone', () => {
    const deep = '?new=1&subject_kind=account&subject_id=acc-1&status=closed';
    expect(jobsFilterSearch(deep, { ...none, status: 'closed' })).toBe(deep);
    expect(jobsFilterSearch(deep, { ...none, status: 'closed', subjectId: 'ast-9' })).toBe(deep);
    expect(jobsFilterSearch(deep, { ...none, status: '' })).toBe(
      '?new=1&subject_kind=account&subject_id=acc-1&status=',
    );
  });

  // And kind, the same way (backlog 3f5cce16): HrPage's deep link names
  // the new job's workflow in `kind`, which the router now reads as the
  // form's Kind under `new=1`. A mount, whose Kind filter is empty, used
  // to take it out of the URL; a Kind chosen while the form is open
  // waits for Cancel.
  test('under new=1, kind is the new job kind and the write leaves it alone', () => {
    const deep = '?new=1&kind=ad-hoc&subject_kind=account&subject_id=acc-1';
    expect(jobsFilterSearch(deep, none)).toBe(deep);
    expect(jobsFilterSearch(deep, { ...none, kind: 'page-audit' })).toBe(deep);
    expect(jobsFilterSearch(deep, { ...none, status: 'closed' })).toBe(`${deep}&status=closed`);
  });
});

// Cancel on a deep-linked form. It stripped the WHOLE query, filters
// included, while the filters stayed set — so the URL and the page
// disagreed — and it relied on the strip to keep the form shut, which
// it did not (backlog d0b93b80). The URL after Cancel is the deep link
// without its new-job half, then the filters written the usual way.
describe('searchWithoutNewJob', () => {
  const none = { kind: '', status: 'open', subjectId: '' };

  test('takes out new, subject_kind and the new job subject, and keeps the filters', () => {
    expect(
      searchWithoutNewJob('?new=1&subject_kind=account&subject_id=acc-1&status=closed', {
        ...none,
        status: 'closed',
      }),
    ).toBe('?status=closed');
  });

  test('keeps every parameter it does not own', () => {
    expect(
      searchWithoutNewJob('?owner_id=emp-1&new=1&subject_kind=vendor&subject_id=v-1', none),
    ).toBe('?owner_id=emp-1');
  });

  test('writes a subject filter typed while the form was open', () => {
    expect(
      searchWithoutNewJob('?new=1&subject_kind=account&subject_id=acc-1', { ...none, subjectId: 'ast-9' }),
    ).toBe('?subject_id=ast-9');
  });

  // The new job's kind is the deep link's too (backlog 3f5cce16): it
  // goes with the rest of the new-job half, and a Kind filter chosen
  // while the form was open is written in its place.
  test('takes out the new job kind with the rest of the new-job half', () => {
    expect(
      searchWithoutNewJob('?new=1&kind=ad-hoc&subject_kind=account&subject_id=acc-1&status=closed', {
        ...none,
        status: 'closed',
      }),
    ).toBe('?status=closed');
    expect(
      searchWithoutNewJob('?new=1&kind=ad-hoc&subject_kind=account&subject_id=acc-1', {
        ...none,
        kind: 'page-audit',
      }),
    ).toBe('?kind=page-audit');
  });

  test('a search with no deep link is only the filters write', () => {
    expect(searchWithoutNewJob('?kind=sale', { ...none, kind: 'sale' })).toBe('?kind=sale');
    expect(searchWithoutNewJob('', none)).toBe('');
  });
});
