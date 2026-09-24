import { describe, expect, test } from 'bun:test';
import { productsSearch } from './searchQuery';

// /ux/products's search box changed component state and never the URL,
// so a reload dropped the query and a filtered view could not be linked
// (backlog 1c2db4c2, page audit 6b4e43a1 gap 8). The page now writes
// back the `q` parseRoute reads — the /ux/jobs mechanism (f8027805) —
// and this is the write, pinned against that read's own rule: an absent
// `q` and an empty one both mean no query.
describe('productsSearch', () => {
  test('an unchanged query leaves the URL exactly as it was', () => {
    // Mount runs the write too: it must not normalise a deep link.
    expect(productsSearch('', '')).toBe('');
    expect(productsSearch('?q=', '')).toBe('?q=');
    expect(productsSearch('?q=stout', 'stout')).toBe('?q=stout');
    expect(productsSearch('?q=pale%20ale', 'pale ale')).toBe('?q=pale%20ale');
  });

  test('a typed query is written, and clearing it takes the parameter back out', () => {
    expect(productsSearch('', 'stout')).toBe('?q=stout');
    expect(productsSearch('?q=stout', 'stou')).toBe('?q=stou');
    expect(productsSearch('?q=stout', '')).toBe('');
  });

  test('a query with spaces and reserved characters reads back as typed', () => {
    const written = productsSearch('', 'pale ale & co?');
    expect(new URLSearchParams(written).get('q')).toBe('pale ale & co?');
  });

  test('parameters the search does not own ride through untouched', () => {
    expect(productsSearch('?from=home', 'can-')).toBe('?from=home&q=can-');
    expect(productsSearch('?from=home&q=can-', '')).toBe('?from=home');
  });
});
