/// The KB page's related-jobs read: the listing's ONE subject filter,
/// `subject_id`, for every entity kind. The page asked `account_id=` /
/// `asset_id=` until 2026-09-23 — aliases the listing never read, so it
/// answered every job; the listing now refuses an unknown parameter
/// with a 400 (backlog 7f3e871a).
export function relatedJobsUrl(entityId: string): string {
  const params = new URLSearchParams({ subject_id: entityId, limit: '50' });
  return `/api/jobs?${params}`;
}
