// What /ux/parts may count, by whether its stock list was read (backlog
// f867d71c; page audit 63d810aa gap 5, 2026-09-23). The inventory starts
// as `[]`, and the header and every filter button derived their numbers
// from it regardless, so while the reads were in flight — and above the
// "Couldn't load parts" line when a row source failed — the page stated
// "0 need attention · 0 out · 0 critical" and All (0): an unread list
// counted as an empty one. A count is printed only for a list that was
// read. Same rule, same shape as /ux/people's roster-counts.ts (backlog
// 47eadca3).

/// Where the page's row reads stand: in flight, failed, or read.
export type StockRead = 'loading' | 'failed' | 'read';

export function stockRead(loading: boolean, loadFailed: string | null): StockRead {
  if (loading) return 'loading';
  return loadFailed === null ? 'read' : 'failed';
}

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
  read: StockRead,
  label: string,
  counts: HeaderCounts,
): Readonly<{ title: string; subtitle: string }> {
  if (read === 'read') {
    return {
      title: `${counts.total} ${label}`,
      subtitle: `${counts.attention} need attention · ${counts.out} out · ${counts.critical} critical`,
    };
  }
  return {
    title: label.charAt(0).toUpperCase() + label.slice(1),
    subtitle: read === 'loading' ? 'Loading stock…' : 'Counts unknown: parts did not load',
  };
}

/// A filter button's label: `Label (n)` for a read list, the bare label
/// otherwise — the button still filters, it just claims no count.
export function countedLabel(label: string, count: number, read: StockRead): string {
  return read === 'read' ? `${label} (${count})` : label;
}
