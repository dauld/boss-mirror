// What /ux/people may count, by whether its roster was read (backlog
// 47eadca3; page audit 0c0265a3 GAP 3, 2026-09-23). The roster starts
// as `[]`, and the header and every filter button derived their numbers
// from it regardless, so while the read was in flight — and above the
// "Couldn't load the roster" line when it failed — the page stated
// "0 active employees" and Active (0): an unread roster counted as an
// empty one. A count is printed only for a roster that was read. The
// read state and the filter-button label are data/readState.ts's
// (`readStateOfLoad`, `countLabel`) — this page and /ux/parts had each
// built their own copies of both (backlog a97d4cf2).

import type { ReadState } from '../data/readState';

/// The page header. A read roster is counted — zero included, because
/// a roster that was read and is empty IS empty; an unread one says
/// why it has no count instead of printing one.
export function rosterHeader(
  read: ReadState,
  activeCount: number,
  expiringCount: number,
): Readonly<{ title: string; subtitle: string }> {
  if (read.kind === 'ok') {
    return {
      title: `${activeCount} active employees`,
      subtitle: `${expiringCount} certifications expiring in 90 days`,
    };
  }
  return {
    title: 'Active employees',
    subtitle:
      read.kind === 'loading' ? 'Loading the roster…' : 'Counts unknown: the roster did not load',
  };
}
