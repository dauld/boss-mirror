// What /ux/people may count, by whether its roster was read (backlog
// 47eadca3; page audit 0c0265a3 GAP 3, 2026-09-23). The roster starts
// as `[]`, and the header and every filter button derived their numbers
// from it regardless, so while the read was in flight — and above the
// "Couldn't load the roster" line when it failed — the page stated
// "0 active employees" and Active (0): an unread roster counted as an
// empty one. A count is printed only for a roster that was read.

/// Where the roster read stands: in flight, failed, or read.
export type RosterRead = 'loading' | 'failed' | 'read';

export function rosterRead(loading: boolean, loadFailed: string | null): RosterRead {
  if (loading) return 'loading';
  return loadFailed === null ? 'read' : 'failed';
}

/// The page header. A read roster is counted — zero included, because
/// a roster that was read and is empty IS empty; an unread one says
/// why it has no count instead of printing one.
export function rosterHeader(
  read: RosterRead,
  activeCount: number,
  expiringCount: number,
): Readonly<{ title: string; subtitle: string }> {
  if (read === 'read') {
    return {
      title: `${activeCount} active employees`,
      subtitle: `${expiringCount} certifications expiring in 90 days`,
    };
  }
  return {
    title: 'Active employees',
    subtitle:
      read === 'loading' ? 'Loading the roster…' : 'Counts unknown: the roster did not load',
  };
}

/// A filter button's label: `Label (n)` for a read roster, the bare
/// label otherwise — the button still filters, it just claims no count.
export function countedLabel(label: string, count: number, read: RosterRead): string {
  return read === 'read' ? `${label} (${count})` : label;
}
