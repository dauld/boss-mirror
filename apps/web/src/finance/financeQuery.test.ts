import { describe, expect, test } from 'bun:test';
import { FINANCE_TABS, financeSearch, readFinanceView, type FinanceView } from './financeQuery';

// /ux/finance kept its tab in component state and read neither ?entry=
// nor ?fact=, though NewJournalEntryPage lands on ?entry=<id> after a
// post and entity-href's ledger-entry and fact kinds target those two
// (backlog 2ab44d55, page audit 3f964c57 gap 7). The read and the write
// below are the page's URL state; the write is the read's inverse, the
// /ux/jobs mechanism (filterQuery.ts, f8027805).
const view = (tab: FinanceView['tab'], entry = '', fact = ''): FinanceView => ({ tab, entry, fact });

describe('readFinanceView', () => {
  test('a bare /ux/finance is the Overview', () => {
    expect(readFinanceView('')).toEqual(view('overview'));
  });

  test('a posted entry lands on the Trial Balance with that entry open', () => {
    expect(readFinanceView('?entry=ent-1')).toEqual(view('trial-balance', 'ent-1'));
    expect(readFinanceView('?fact=fact-1')).toEqual(view('trial-balance', '', 'fact-1'));
  });

  test('a named tab is read as named, and an unknown one as the default', () => {
    expect(readFinanceView('?tab=invoices')).toEqual(view('invoices'));
    expect(readFinanceView('?tab=nonsense')).toEqual(view('overview'));
    expect(readFinanceView('?tab=nonsense&entry=e')).toEqual(view('trial-balance', 'e'));
  });

  test('every tab the page draws is a tab the URL can name', () => {
    for (const t of FINANCE_TABS) {
      expect(readFinanceView(`?tab=${t.id}`).tab).toBe(t.id);
    }
  });
});

describe('financeSearch', () => {
  test('mounting rewrites nothing — the deep link stays as it was opened', () => {
    for (const s of ['', '?entry=ent-1', '?fact=f', '?tab=invoices', '?tab=nonsense', '?from=home&tab=cash-flow']) {
      expect(financeSearch(s, readFinanceView(s))).toBe(s);
    }
  });

  test('a chosen tab is written, and the Overview takes the parameter back out', () => {
    expect(financeSearch('', view('invoices'))).toBe('?tab=invoices');
    expect(financeSearch('?tab=invoices', view('overview'))).toBe('');
  });

  test('leaving the Trial Balance for another tab drops the entry it had open', () => {
    expect(financeSearch('?entry=ent-1', view('overview'))).toBe('');
    expect(financeSearch('?entry=ent-1', view('invoices'))).toBe('?tab=invoices');
  });

  test('closing the entry keeps the Trial Balance, now named', () => {
    expect(financeSearch('?entry=ent-1', view('trial-balance'))).toBe('?tab=trial-balance');
  });

  test('parameters the page does not own ride through', () => {
    expect(financeSearch('?from=home', view('cash-flow'))).toBe('?from=home&tab=cash-flow');
    expect(financeSearch('?from=home&tab=cash-flow', view('overview'))).toBe('?from=home');
  });

  test('every view reads back as written', () => {
    for (const t of FINANCE_TABS) {
      for (const [entry, fact] of [['', ''], ['e 1&x', ''], ['', 'f/2'], ['e', 'f']] as const) {
        const v = view(t.id, entry, fact);
        expect(readFinanceView(financeSearch('?from=home', v))).toEqual(v);
      }
    }
  });
});
