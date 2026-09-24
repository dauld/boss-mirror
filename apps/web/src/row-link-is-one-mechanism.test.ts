import { describe, expect, test } from 'bun:test';
import { readdirSync, readFileSync } from 'node:fs';
import { join, relative } from 'node:path';

/** A clickable table row has ONE mechanism: the web-kit `rowLink` action
 *  (backlog 2361ac45 and 96490f23, decided 2026-09-24).
 *
 *  `data-table-row-link` is the class every stylesheet keys a clickable
 *  row's look on — pointer, hover wash, focus ring. While pages typed it
 *  by hand, a row could wear it and do nothing: /ux/parts and /ux/people
 *  rows navigated only from their id cell, three warehouse tables and two
 *  support tables not at all, and the rows that did navigate did it from
 *  a hand-rolled onclick with no keyboard path. `rowLink` sets the class
 *  itself, together with the click, the role, the tab stop and Enter/Space,
 *  so a row looks clickable exactly when it is. These two pins hold every
 *  page to that: nobody else writes the class, and no <tr> navigates from
 *  its own onclick. A row that toggles in place (TrialBalanceTab) keeps its
 *  onclick — it calls no `navigate`. */

const SRC = import.meta.dir;

function svelteFiles(dir: string): ReadonlyArray<string> {
  return readdirSync(dir, { withFileTypes: true }).flatMap((entry) => {
    const path = join(dir, entry.name);
    if (entry.isDirectory()) return svelteFiles(path);
    return entry.name.endsWith('.svelte') ? [path] : [];
  });
}

const FILES: ReadonlyArray<Readonly<{ name: string; text: string }>> = svelteFiles(SRC).map((path) => ({
  name: relative(SRC, path),
  text: readFileSync(path, 'utf8'),
}));

/** The class written into markup: a class attribute or a class: directive
 *  naming it (a comment mentioning it is not a use). */
const HAND_SET_CLASS = /class(?:="[^"]*|=\{[^}]*|:)data-table-row-link/;

/** A <tr> whose own onclick navigates. */
const ROW_ONCLICK_NAVIGATES = /<tr\b[^>]*?\bonclick=\{[^}]*\bnavigate\(/;

describe('a clickable table row is one mechanism', () => {
  test('the pin reads the tree it pins', () => {
    // Control: the walk must reach the pages this was filed against.
    const names = FILES.map((f) => f.name);
    expect(names).toContain(join('parts', 'PartsList.svelte'));
    expect(names).toContain(join('people', 'PeopleList.svelte'));
    expect(HAND_SET_CLASS.test('<tr class="data-table-row-link">')).toBe(true);
    expect(ROW_ONCLICK_NAVIGATES.test('<tr\n  class="x"\n  onclick={() => navigate(to)}\n>')).toBe(true);
    expect(ROW_ONCLICK_NAVIGATES.test('<tr onclick={() => toggleAccount(code)}>')).toBe(false);
  });

  test('only rowLink sets data-table-row-link', () => {
    const offenders = FILES.filter((f) => HAND_SET_CLASS.test(f.text)).map((f) => f.name);
    expect(offenders).toEqual([]);
  });

  test('no table row navigates from a hand-rolled onclick', () => {
    const offenders = FILES.filter((f) => ROW_ONCLICK_NAVIGATES.test(f.text)).map((f) => f.name);
    expect(offenders).toEqual([]);
  });
});
