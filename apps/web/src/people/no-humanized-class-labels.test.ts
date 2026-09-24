// No component labels a Class code by title-casing it.
//
// A department or role code has a label in the Class registry, its
// display_name, and classLabel (./types) is the one function that reads
// it — humanizing only a code the registry lacks, or every code while
// it loads. Backlog 8a331c9b moved the /ux/people roster onto it and
// found five more surfaces still calling humanizeClassCode directly
// (backlog 8677728c): the Hierarchy role line, four places on the
// employee page, HR's headcount table and QA's staffing table, so the
// live `operations` department printed Operations where its Class says
// Operations / IT. Fixing six sites by hand and leaving a comment is
// how the other five were missed; this scan names the next one.
//
// The rule is scoped to .svelte, where labels are rendered. The pure
// helpers in .ts keep the fallback — classLabel itself, and the Status
// buttons' bucket for a status the registry lacks — and are unit-tested
// beside it.

import { describe, expect, test } from 'bun:test';
import { readdirSync, readFileSync, statSync } from 'node:fs';
import { join } from 'node:path';

/// Every `.svelte` file under a root, recursively.
function svelteFiles(dir: string): string[] {
  const out: string[] = [];
  for (const entry of readdirSync(dir)) {
    if (entry === 'node_modules' || entry === '.svelte-kit') continue;
    const full = join(dir, entry);
    if (statSync(full).isDirectory()) out.push(...svelteFiles(full));
    else if (entry.endsWith('.svelte')) out.push(full);
  }
  return out;
}

const ROOT = new URL('../', import.meta.url).pathname; // apps/web/src

/// The call, with its paren: prose may name the function freely.
const CALL = /\bhumanizeClassCode\s*\(/;

/// The files that call it, as `src/…` paths.
function offenders(files: ReadonlyArray<string>, read: (f: string) => string): string[] {
  return files
    .filter((f) => CALL.test(read(f)))
    .map((f) => f.replace(/.*\/apps\/web\//, ''))
    .sort();
}

describe('no .svelte file humanizes a Class code itself', () => {
  test('every department and role label goes through classLabel', () => {
    const found = offenders(svelteFiles(ROOT), (f) => readFileSync(f, 'utf8'));
    expect(
      found,
      "label a Class code with classLabel(code, classesFor('employee', '<attribute>')) " +
        'from ./people/types — it reads the registry display_name and humanizes only a code the registry lacks',
    ).toEqual([]);
  });

  test('the scan reads real files and would name a call', () => {
    // A moved directory or a changed extension empties the list and
    // passes the test above vacuously; a pattern that never matches
    // does the same.
    expect(svelteFiles(ROOT).length).toBeGreaterThan(50);
    const planted = '/x/apps/web/src/people/Planted.svelte';
    expect(offenders([planted], () => '<td>{humanizeClassCode(e.role)}</td>')).toEqual([
      'src/people/Planted.svelte',
    ]);
    expect(offenders([planted], () => '<!-- not humanizeClassCode -->')).toEqual([]);
  });
});
