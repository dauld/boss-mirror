// THE ONE PHONE BREAKPOINT (design 62de32ae decision 12, car G).
//
// Measured 2026-09-24 at 390px: no surface had a phone layout, so /it
// rendered 1246px wide and the tab bar's own tab, search and sign-in
// sat past the right edge of the screen. At or under this width the
// chrome bar scrolls sideways inside itself (PerspectiveTabs and the
// panels it opens), the app shell's sidebar collapses into one row
// (apps/web styles.css), and the IT world is drawn as a strip rather
// than the SVG shrunk (apps/web it/yard/MapPage.svelte).
//
// A media query cannot import a constant, so each of those places
// spells the width in CSS — and phone.test.ts here, and
// phone-strip.test.ts in apps/web, hold every one of them equal to this
// (CLAUDE.md 9a: a fact that lives twice gets an equality test). 720px
// is the width car F's HUD frame stacks its rows at, so the phone
// layouts turn over together.

export const PHONE_MAX_WIDTH = 720;
export const PHONE_QUERY = `(max-width: ${PHONE_MAX_WIDTH}px)`;
