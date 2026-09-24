// The /ux/finance view, read off and written back to the URL (backlog
// 2ab44d55, page audit 3f964c57 gap 7).
//
// The active tab lived in component state only, so back from an
// invoice and a reload both returned to Overview; and the page read
// neither ?entry= nor ?fact=, though NewJournalEntryPage lands on
// ?entry=<id> after a post and entity-href's ledger-entry and fact
// kinds link there — so the operator who had just posted an entry was
// shown the Overview instead of it. parseRoute reads this view off the
// query and App mounts FinancePage with it; the page writes it back
// with replaceState, the /ux/jobs mechanism (../jobs/filterQuery.ts,
// f8027805). The write is the read's inverse, so a mount rewrites
// nothing.
//
// An entry or a fact is a drill-down OF the Trial Balance, so either
// one without a `tab` means that tab; with neither, an absent `tab`
// means Overview.

/** The page's tabs, as data: FinancePage draws this list and the read
 *  below accepts exactly these ids, so the two cannot disagree. */
export const FINANCE_TABS = [
  { id: 'overview', label: 'Overview' },
  { id: 'invoices', label: 'Invoices' },
  { id: 'approvals', label: 'PO Approvals' },
  { id: 'income-statement', label: 'Income statement' },
  { id: 'balance-sheet', label: 'Balance sheet' },
  { id: 'cash-flow', label: 'Cash flow' },
  { id: 'trial-balance', label: 'Trial Balance' },
  { id: 'tax-liability', label: 'Tax liability' },
] as const;

export type FinanceTab = (typeof FINANCE_TABS)[number]['id'];

/** `entry` is a journal entry id, `fact` a financial fact id; empty is none. */
export type FinanceView = Readonly<{ tab: FinanceTab; entry: string; fact: string }>;

function isTab(s: string): s is FinanceTab {
  return FINANCE_TABS.some((t) => t.id === s);
}

/** What an absent `tab` means, given the entry and fact beside it. */
function tabAbsentMeans(entry: string, fact: string): FinanceTab {
  return entry || fact ? 'trial-balance' : 'overview';
}

export function readFinanceView(search: string): FinanceView {
  const params = new URLSearchParams(search);
  const entry = params.get('entry') ?? '';
  const fact = params.get('fact') ?? '';
  const tab = params.get('tab') ?? '';
  return { tab: isTab(tab) ? tab : tabAbsentMeans(entry, fact), entry, fact };
}

/** The search string that makes readFinanceView read `v` back, built
 *  from `search`. Returns `search` itself when it already reads as `v`,
 *  so a mount never rewrites the deep link it was opened from, and
 *  every parameter the page does not own rides through. */
export function financeSearch(search: string, v: FinanceView): string {
  const read = readFinanceView(search);
  if (read.tab === v.tab && read.entry === v.entry && read.fact === v.fact) return search;
  const params = new URLSearchParams(search);
  const put = (key: string, want: string): void => {
    if (want) params.set(key, want);
    else params.delete(key);
  };
  put('entry', v.entry);
  put('fact', v.fact);
  put('tab', v.tab === tabAbsentMeans(v.entry, v.fact) ? '' : v.tab);
  const s = params.toString();
  return s ? `?${s}` : '';
}
