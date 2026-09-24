// Svelte action for clickable table rows — the click + role +
// tabindex + Enter/Space pattern tables hand-roll on <tr> (see
// finance/TrialBalanceTab for the ancestor), packaged once:
//
//   <tr use:rowLink={{ onActivate: () => navigate(href), label: name }}>
//
// It is the ONE mechanism for a row that navigates (backlog 2361ac45,
// decided 2026-09-24). Until then `data-table-row-link` was a class a
// page typed by hand, so a row could be styled clickable — pointer,
// hover wash, focus ring — and do nothing (/ux/parts, /ux/people, the
// warehouse and support tables), or navigate from a hand-rolled
// onclick with no keyboard path at all. The action now sets that class
// itself and nothing else may (pinned in apps/web by
// src/row-link-is-one-mechanism.test.ts), so a row looks clickable
// exactly when it is.
//
// A click that starts inside a link or another control in the row is
// that control's alone: the row answering it too pushed two history
// entries for one click (backlog 18890a16). Link.svelte also stops its
// own click, but a row must not depend on every anchor inside it
// knowing to — a plain <a> or a <button> in a cell is enough.
//
// Navigation stays the caller's job (pass a closure over the app's
// `navigate`) so web-kit doesn't grow a router dependency. Rows
// that *navigate* get role="link"; rows that toggle in place should
// keep a hand-rolled role="button" instead.

export type RowLinkParams = Readonly<{
  /// Fires on click, Enter, and Space.
  onActivate: () => void;
  /// Accessible name for the row link; screen readers otherwise
  /// read the entire row content.
  label?: string;
}>;

/// The class every stylesheet keys a clickable row's look on.
export const ROW_LINK_CLASS = 'data-table-row-link';

/// What owns its own click when it sits inside a row.
const OWNS_ITS_CLICK = 'a[href], button, input, select, textarea, label, summary';

export function rowLink(
  node: HTMLElement,
  params: RowLinkParams,
): { update(next: RowLinkParams): void; destroy(): void } {
  let current = params;

  node.classList.add(ROW_LINK_CLASS);
  node.setAttribute('role', 'link');
  node.tabIndex = 0;

  function applyLabel(): void {
    if (current.label) {
      node.setAttribute('aria-label', current.label);
    } else {
      node.removeAttribute('aria-label');
    }
  }
  applyLabel();

  function onClick(e: MouseEvent): void {
    const target = e.target as Element | null;
    const owner = target && typeof target.closest === 'function' ? target.closest(OWNS_ITS_CLICK) : null;
    if (owner && owner !== node) return;
    current.onActivate();
  }
  function onKeydown(e: KeyboardEvent): void {
    // Only the row's own key: Enter on a link inside it is already that
    // link's click, and would otherwise navigate a second time.
    if (e.target !== node) return;
    if (e.key === 'Enter' || e.key === ' ') {
      e.preventDefault();
      current.onActivate();
    }
  }
  node.addEventListener('click', onClick);
  node.addEventListener('keydown', onKeydown);

  return {
    update(next: RowLinkParams): void {
      current = next;
      applyLabel();
    },
    destroy(): void {
      node.removeEventListener('click', onClick);
      node.removeEventListener('keydown', onKeydown);
    },
  };
}
