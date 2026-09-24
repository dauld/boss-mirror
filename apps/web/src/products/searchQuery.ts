// The /ux/products search query, written back to the URL (backlog
// 1c2db4c2, page audit 6b4e43a1 gap 8).
//
// The search box filtered in component state and never touched the
// query, so a reload came back unfiltered and a filtered view could not
// be linked. parseRoute in ../router.ts now reads `q` off /products and
// App mounts ProductsList with it; this is the inverse of that read, the
// mechanism /ux/jobs's filters use (../jobs/filterQuery.ts, f8027805):
// an absent `q` and an empty one both mean no query.

/** The search string that makes parseRoute read `query` back, built from
 *  `search` by touching `q` only when its read value differs. Returns
 *  `search` itself when nothing differs, so a mount never rewrites the
 *  deep link it was opened from, and every other parameter rides through. */
export function productsSearch(search: string, query: string): string {
  const params = new URLSearchParams(search);
  if ((params.get('q') ?? '') === query) return search;
  if (query === '') params.delete('q');
  else params.set('q', query);
  const s = params.toString();
  return s ? `?${s}` : '';
}
