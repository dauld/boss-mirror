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

/// One read's outcome. A failure carries its reason, because the
/// operator reading the page is the person who has to act on it.
export type ReadState =
  | { kind: 'ok' }
  | { kind: 'failed'; error: string };

export const okRead: ReadState = { kind: 'ok' };

export function failedRead(error: string): ReadState {
  return { kind: 'failed', error };
}

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
/// the same.
export function orderedHealthRows<
  T extends { openCount: number; account: { name: string } },
>(rows: ReadonlyArray<T>): T[] {
  return [...rows].sort(
    (a, b) =>
      b.openCount - a.openCount || a.account.name.localeCompare(b.account.name),
  );
}
