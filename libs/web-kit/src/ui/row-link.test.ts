// Unit tests for the rowLink action — the ONE mechanism a clickable
// table row uses (backlog 2361ac45 / 96490f23, decided 2026-09-24).
// Run via `bun test`.
//
// Bun has no DOM, and the action touches only a handful of node
// methods, so the row here is a recording fake rather than a jsdom
// dependency: what it records is exactly what the action is for —
// the class that styles a row as clickable, the role and tab stop
// that make it reachable, and how many times one gesture navigates.

import { describe, expect, test } from 'bun:test';
import { rowLink } from './RowLink';

type Handler = (e: unknown) => void;

function fakeRow() {
  const listeners = new Map<string, Handler>();
  const attrs = new Map<string, string>();
  const classes = new Set<string>();
  const node = {
    tabIndex: -1,
    setAttribute: (k: string, v: string) => void attrs.set(k, v),
    removeAttribute: (k: string) => void attrs.delete(k),
    classList: { add: (c: string) => void classes.add(c) },
    addEventListener: (t: string, f: Handler) => void listeners.set(t, f),
    removeEventListener: (t: string) => void listeners.delete(t),
    // A click on a bare cell: nothing interactive between it and the row.
    closest: (_sel: string) => node,
  };
  const fire = (type: string, e: unknown) => listeners.get(type)?.(e);
  return { node, attrs, classes, listeners, fire };
}

/** A click whose target sits inside `hit` (an <a>, a <button> …), or
 *  directly on a cell when `hit` is the row itself. */
function clickFrom(hit: unknown) {
  return { target: { closest: (_sel: string) => hit } };
}

function mount(label?: string) {
  const row = fakeRow();
  let navigations = 0;
  const action = rowLink(row.node as unknown as HTMLElement, {
    onActivate: () => {
      navigations += 1;
    },
    label,
  });
  return { ...row, action, navigations: () => navigations };
}

describe('rowLink', () => {
  test('marks the row as a link: the styling class, role, tab stop and name', () => {
    const r = mount('Tri-clamp gasket (SP-GASKET-01)');
    // The class is the action's to set — a row styled clickable by hand
    // but never wired was the defect (2361ac45, 96490f23).
    expect(r.classes.has('data-table-row-link')).toBe(true);
    expect(r.attrs.get('role')).toBe('link');
    expect(r.node.tabIndex).toBe(0);
    expect(r.attrs.get('aria-label')).toBe('Tri-clamp gasket (SP-GASKET-01)');
  });

  test('a click on a cell navigates once', () => {
    const r = mount();
    r.fire('click', clickFrom(r.node));
    expect(r.navigations()).toBe(1);
  });

  test('a click inside a link in the row is the link’s alone', () => {
    const r = mount();
    const anchor = { tagName: 'A' };
    r.fire('click', clickFrom(anchor));
    expect(r.navigations()).toBe(0);
  });

  test('a click inside any other control in the row is that control’s alone', () => {
    const r = mount();
    r.fire('click', clickFrom({ tagName: 'BUTTON' }));
    expect(r.navigations()).toBe(0);
  });

  test('Enter and Space on the focused row navigate once each', () => {
    const r = mount();
    let prevented = 0;
    const key = (k: string) => ({ key: k, target: r.node, preventDefault: () => void (prevented += 1) });
    r.fire('keydown', key('Enter'));
    r.fire('keydown', key(' '));
    r.fire('keydown', key('a'));
    expect(r.navigations()).toBe(2);
    expect(prevented).toBe(2);
  });

  test('Enter on a link inside the row does not also activate the row', () => {
    // The browser turns Enter on the <a> into the <a>'s own click; the
    // keydown still bubbles here, and answering it would navigate twice.
    const r = mount();
    r.fire('keydown', { key: 'Enter', target: { tagName: 'A' }, preventDefault: () => {} });
    expect(r.navigations()).toBe(0);
  });

  test('update swaps the target and the name; destroy detaches both listeners', () => {
    const r = mount('old');
    let second = 0;
    r.action.update({ onActivate: () => void (second += 1), label: undefined });
    expect(r.attrs.has('aria-label')).toBe(false);
    r.fire('click', clickFrom(r.node));
    expect(second).toBe(1);
    expect(r.navigations()).toBe(0);
    r.action.destroy();
    expect(r.listeners.size).toBe(0);
  });
});
