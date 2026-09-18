// The `ad-hoc` Workflow the jobs list's "Create Ad Hoc Job" preselects.
// Every shipped tenant registers one (brewery + device-shop seeds, #92),
// but a registry is data and a tenant may not: until 2026-09-18 the
// button presumed the row and, under a registry without it, the click
// changed nothing visible — the interaction crawl (car f2b8a01c,
// KNOWN_GAPS #6) reported it on /ux/jobs, /ux/service and /ux/sales
// (backlog a399613d). The button now renders off this read.

export const AD_HOC_KIND = 'ad-hoc';

/** The registered ad-hoc Workflow row, or null when the registry
 *  (as loaded so far) carries none. */
export function registeredAdHoc<K extends { kind: string }>(kinds: ReadonlyArray<K>): K | null {
  return kinds.find((k) => k.kind === AD_HOC_KIND) ?? null;
}
