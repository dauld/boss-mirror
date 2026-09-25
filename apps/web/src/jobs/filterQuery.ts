// The /ux/jobs filters, written back to the URL (backlog f8027805).
//
// parseRoute in ../router.ts reads `kind`, `status` and `subject_id`
// off the /jobs query and App mounts JobsListPage with them; the
// filters changed page state and never that query, so a reload fell
// back to Open and a filtered view could not be shared. This is the
// inverse of that read, and it follows the read's own rules rather
// than a second set: an absent status means JOBS_DEFAULT_STATUS, an
// EMPTY one means every status (backlog 03e198e5), an empty kind or
// subject id is no filter at all, and under `new=1` the kind and the
// subject id are the new job's and no filter (d0b93b80, 3f5cce16).

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
  // Under `new=1` parseRoute reads kind and subject_id as the new job's
  // Kind and subject, not as these filters (backlog d0b93b80 for the
  // subject, 3f5cce16 for the kind), so the write leaves both be: a
  // mount must not strip the deep link's new job, and a filter set
  // while that form is open is written by its Cancel.
  const deepLink = isNewJobDeepLink(params);
  if (!deepLink) put('kind', params.get('kind') ?? '', f.kind, '');
  put('status', params.get('status') ?? JOBS_DEFAULT_STATUS, f.status, JOBS_DEFAULT_STATUS);
  if (!deepLink) put('subject_id', params.get('subject_id') ?? '', f.subjectId, '');
  if (!changed) return search;
  const s = params.toString();
  return s ? `?${s}` : '';
}

/** The parameters a `new=1` deep link carries for the job it opens. */
const NEW_JOB_PARAMS = ['new', 'kind', 'subject_kind', 'subject_id'] as const;

function isNewJobDeepLink(params: URLSearchParams): boolean {
  return params.get('new') === '1';
}

/** The search Cancel leaves on a deep-linked new-job form: the deep
 *  link without its new-job half, then the filters written as usual.
 *  Cancel stripped the WHOLE query until backlog d0b93b80 — the
 *  filters' parameters with it, while the filters stayed set — so the
 *  URL and the page disagreed. A search with no deep link is only the
 *  filters' write. */
export function searchWithoutNewJob(search: string, f: JobsFilters): string {
  const params = new URLSearchParams(search);
  if (!isNewJobDeepLink(params)) return jobsFilterSearch(search, f);
  for (const key of NEW_JOB_PARAMS) params.delete(key);
  const s = params.toString();
  return jobsFilterSearch(s ? `?${s}` : '', f);
}
