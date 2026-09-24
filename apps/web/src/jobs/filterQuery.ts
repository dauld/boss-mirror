// The /ux/jobs filters, written back to the URL (backlog f8027805).
//
// parseRoute in ../router.ts reads `kind`, `status` and `subject_id`
// off the /jobs query and App mounts JobsListPage with them; the
// filters changed page state and never that query, so a reload fell
// back to Open and a filtered view could not be shared. This is the
// inverse of that read, and it follows the read's own rules rather
// than a second set: an absent status means JOBS_DEFAULT_STATUS, an
// EMPTY one means every status (backlog 03e198e5), and an empty kind
// or subject id is no filter at all.

/** What an absent `status` parameter means on /jobs. App mounts the
 *  page with it, and the write below leaves the parameter out for it,
 *  so the two cannot disagree about what a bare /jobs shows. */
export const JOBS_DEFAULT_STATUS = 'open';

export type JobsFilters = Readonly<{ kind: string; status: string; subjectId: string }>;

/** The search string that makes parseRoute read `f` back, built from
 *  `search` by touching only the parameters whose read value differs.
 *  Returns `search` itself when nothing differs, so a mount never
 *  rewrites the deep link it was opened from, and every parameter the
 *  filters do not own (owner_id, kind_prefix, new, …) rides through. */
export function jobsFilterSearch(search: string, f: JobsFilters): string {
  const params = new URLSearchParams(search);
  let changed = false;
  const put = (key: string, read: string, want: string, absentMeans: string): void => {
    if (read === want) return;
    changed = true;
    if (want === absentMeans) params.delete(key);
    else params.set(key, want);
  };
  put('kind', params.get('kind') ?? '', f.kind, '');
  put('status', params.get('status') ?? JOBS_DEFAULT_STATUS, f.status, JOBS_DEFAULT_STATUS);
  put('subject_id', params.get('subject_id') ?? '', f.subjectId, '');
  if (!changed) return search;
  const s = params.toString();
  return s ? `?${s}` : '';
}
