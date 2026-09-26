// What /ux/parts may count, by whether its stock list was read (backlog
// f867d71c; page audit 63d810aa gap 5, 2026-09-23). The inventory starts
// as `[]`, and the header and every filter button derived their numbers
// from it regardless, so while the reads were in flight — and above the
// "Couldn't load parts" line when a row source failed — the page stated
// "0 need attention · 0 out · 0 critical" and All (0): an unread list
// counted as an empty one. A count is printed only for a list that was
// read. The read state and the filter-button label are
// data/readState.ts's (`readStateOfLoad`, `countLabel`) — this page and
// /ux/people had each built their own copies of both (backlog a97d4cf2).

import type { ReadState } from '../data/readState';

export type HeaderCounts = Readonly<{
  total: number;
  attention: number;
  out: number;
  critical: number;
}>;

/// The page header. `label` is the tenant's name for its parts
/// (`parts.page_title`). A read list is counted — zero included, because
/// an inventory that was read and is empty IS empty; an unread one says
/// why it has no count instead of printing one. That covers a failed
/// catalog read beside a good inventory read too: the rows exist, but
/// the page is showing a failure, not them.
export function partsHeader(
  read: ReadState,
  label: string,
  counts: HeaderCounts,
): Readonly<{ title: string; subtitle: string }> {
  if (read.kind === 'ok') {
    return {
      title: `${counts.total} ${label}`,
      subtitle: `${counts.attention} need attention · ${counts.out} out · ${counts.critical} critical`,
    };
  }
  return {
    title: label.charAt(0).toUpperCase() + label.slice(1),
    subtitle: read.kind === 'loading' ? 'Loading stock…' : 'Counts unknown: parts did not load',
  };
}
