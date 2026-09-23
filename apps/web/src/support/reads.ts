// What each of the support page's reads did, and what the page is
// therefore allowed to say.
//
// WHY THIS IS A MODULE AND NOT THREE TERNARIES IN THE COMPONENT. The
// page already learned this lesson once: packet 3fba9c35 added
// `loadFailed` for the cases read, so a cases outage stops rendering as
// "no open cases". Its two SECONDARY reads kept the old shape right
// beside that fix —
//
//   const pBody = pResp.ok ? await pResp.json() : [];            // accounts
//   devicesPage = dPaged.kind === 'ready' ? dPaged.page : null;  // devices
//
// — and each manufactures the same silence the first one was fixed for
// (packets 325f34cd and 8c9e1190, the /ux/support page audit). Written
// as an expression inside the effect, the mistake is one character of
// punctuation and invisible in review. Written here, the question "does
// a failed read look different from an empty one" is a function with a
// name and a test.

// ReadState, okRead and failedRead were defined here and now live in
// ../data/readState, lifted by packet 7a7bfc88: the same defect is live
// in six more pages, and six copies of one idea is the drift CLAUDE.md
// 9a is about. Re-exported because this page and its test already
// import them from here; the definition is one file, not seven.
import type { ReadState } from '../data/readState';
import { compareSortValues } from '@boss/web-kit/ui/sort';
export { failedRead, okRead, type ReadState } from '../data/readState';

/// What the Account Health tab renders. The tab's rows are built from
/// the accounts list, so an accounts outage produces zero rows — which
/// before this was indistinguishable from a tenant with no accounts.
export type AccountHealthView =
  | { kind: 'cases-failed'; error: string }
  | { kind: 'accounts-failed'; error: string }
  | { kind: 'empty' }
  | { kind: 'rows' };

export function accountHealthView(
  cases: ReadState,
  accounts: ReadState,
  rowCount: number,
): AccountHealthView {
  // Cases first: it is the page's primary dataset and its failure is
  // already the page's failure everywhere else.
  if (cases.kind === 'failed') {
    return { kind: 'cases-failed', error: cases.error };
  }
  // BEFORE the row count, deliberately. A failed accounts read can
  // still leave rows standing from a partial join, and reporting those
  // numbers as if they were complete is the quieter half of this bug.
  if (accounts.kind === 'failed') {
    return { kind: 'accounts-failed', error: accounts.error };
  }
  return rowCount === 0 ? { kind: 'empty' } : { kind: 'rows' };
}

/// What an em-dash in a device cell means. With the failed arm
/// discarded there was no answer to this: `isCapped(null)` is false, so
/// the overflow banner could not fire either, and the page had no
/// surface at all that could report the assets read had failed.
export function deviceCellMeaning(devices: ReadState): 'no-device' | 'unknown' {
  return devices.kind === 'failed' ? 'unknown' : 'no-device';
}

/// The Account Health tab's rows, ordered — every account, not only the
/// ones in trouble.
///
/// WHY THE FILTER WENT (packet 4c708662). The tab filtered to
/// `openCount > 0`, and openCount counts open field-service jobs, which
/// are structurally zero because no such workflow is published. So the
/// tab rendered "No account data." while account data existed — the
/// surface's words drifted from the fact they describe, which is the
/// Orwell clause in the guidelines read literally.
///
/// The packet offers two readings — the words are wrong, or the filter
/// is — and the table settles it: its columns are account, tier,
/// openCount, deviceCount and lastDate. Three of the five say something
/// about an account with no open case, and a tab called Account Health
/// that hides every healthy account hides the roster. Removing the
/// filter also makes the empty state true again, rather than needing
/// its own reworded sentence.
///
/// ORDERED, not merely unfiltered: accounts with open cases sort first,
/// so the worklist reading the filter used to give is kept rather than
/// traded away. Ties break by name so two reads of the same data look
/// the same. An unnamed account (the accounts domain is identity-first,
/// so name is null until enriched) ties before the named ones, by
/// web-kit's no-data-first convention rather than by throwing
/// (backlog ae7d1ce4).
export function orderedHealthRows<
  T extends { openCount: number; account: { name: string | null } },
>(rows: ReadonlyArray<T>): T[] {
  return [...rows].sort(
    (a, b) =>
      b.openCount - a.openCount ||
      compareSortValues(a.account.name, b.account.name),
  );
}

/// How long a support case may stay open before the page calls it
/// escalated.
///
/// WHY IT IS HERE AND NOT A LITERAL (packet e0a40c81, gap 11 of the
/// /ux/support audit). `daysOpen > 14` decided the escalatedCount tile
/// AND the amber row styling, with the tile's label spelling a third
/// 14 in prose. No workflow row, Class row or metadata field declares
/// that rule, so the page was inventing an operating rule a tenant
/// cannot change without editing the frontend — Registries Over
/// Hardcoded Paths in the smallest possible way.
///
/// THIS IS NOT THE FIX, IT IS THE HOLDING ACTION. The threshold belongs
/// on the workflow that defines a support case, and no such workflow is
/// published (gap 1 of the same audit), so there is nothing yet to
/// carry it. Named here, the number lives once and its value is pinned
/// by a test, so it cannot drift silently between its call sites in the
/// meantime. When the workflow lands, this constant becomes that field's
/// default and the call sites keep their shape.
export const ESCALATION_DAYS = 14;

/// Whether a case open this many days counts as escalated. Strictly
/// greater: the 14th day is not yet escalated, which is what the two
/// `> 14` expressions encoded and neither of them said.
export function isEscalated(daysOpen: number): boolean {
  return daysOpen > ESCALATION_DAYS;
}
