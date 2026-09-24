<script lang="ts">
  // The chrome bar — the one thing every app shares.
  //
  // Tenant wordmark (left), the app tabs (centre), and the shared
  // right-hand controls: global search, feedback, system time and the
  // sign-in/out control.
  // Everything below it belongs to whichever app is active; this bar
  // is the only fixed furniture. 44px tall — each app's shell offsets
  // its own chrome below it.
  //
  // The tab list is the host's: apps/web derives it from the tenant's
  // Class registry and manifest (one tab per declared department,
  // Simulator only when the `sim` module is on — ce68f137), and
  // apps/simulator uses the default. Tabs are plain anchors: Simulator
  // is served by a different piece (boss-simulator) so switching to it
  // is a real navigation, and for the same-SPA apps the router picks
  // the change up on popstate.
  import SystemTime from './SystemTime.svelte';
  import SignInControl from './SignInControl.svelte';
  import GlobalSearch from './GlobalSearch.svelte';
  import FeedbackControl from './FeedbackControl.svelte';
  import LoopMark from './ui/LoopMark.svelte';
  import { APPS as DEFAULT_APPS, type AppId, type AppTab } from './nav';
  import { manifest } from './session/manifest.svelte';
  import { session } from './session/session.svelte';

  let {
    active,
    apps = DEFAULT_APPS,
    brandName: brandNameProp,
    brandSub: brandSubProp,
    searchAppKinds = [] as ReadonlyArray<string>,
  } = $props<{
    active: AppId;
    /// Every app this host offers. Defaults to Home + Simulator, which
    /// is all apps/simulator has; apps/web passes the full list built
    /// from its nav catalog.
    apps?: ReadonlyArray<AppTab>;
    /// Overrides the tenant's own name. Left unset everywhere in the
    /// shipped apps — the brand comes from the manifest, because
    /// hardcoding it is how three render sites came to disagree and
    /// how a second tenant ended up showing a brewery's name.
    brandName?: string;
    brandSub?: string;
    /// Subject kinds of the active app, passed to global search as a
    /// ranking hint. web-kit deliberately does not know the mapping —
    /// the host owns which surfaces belong to which app.
    searchAppKinds?: ReadonlyArray<string>;
  }>();

  /// The tenant's own name, split on its last word so "Algedonic Ales"
  /// still renders as a wordmark plus a lighter suffix — the shape the
  /// hardcoded props produced, now derived rather than repeated.
  ///
  /// Falls back to "BOSS" while the manifest loads and for a
  /// deployment that has not named itself. A prop still wins, for a
  /// host that genuinely needs to override.
  let brand = $derived.by(() => {
    const name =
      brandNameProp !== undefined
        ? undefined
        : manifest.value.kind === 'ready'
          ? manifest.value.displayName
          : undefined;
    if (brandNameProp !== undefined) {
      return { name: brandNameProp, sub: brandSubProp ?? '' };
    }
    if (!name) return { name: 'BOSS', sub: '' };
    const words = name.trim().split(/\s+/);
    return words.length > 1
      ? { name: words.slice(0, -1).join(' '), sub: words[words.length - 1]! }
      : { name, sub: '' };
  });
  let brandName = $derived(brand.name);
  let brandSub = $derived(brand.sub);

  // Which tabs sit on the bar, and which fold into "More".
  //
  // There are as many department apps as the tenant declares
  // departments — seventeen for the playground tenant — and a bar of
  // seventeen tabs is a bar nobody reads. But hiding your OWN
  // department behind a menu is worse.
  //
  // So the bar carries exactly Home, Simulator (when the tenant has
  // one) and your department, on every surface, always. It does NOT
  // pin the app you happen to be in: that was the first attempt and
  // it made the tab set change as you navigated, which is precisely
  // the drift `chrome-consistency.mocked.spec.ts` exists to catch — a
  // bar you cannot build muscle memory against.
  //
  // Orientation instead comes from the More control, which shows where
  // you are when where you are is inside it. The set stays fixed; only
  // the label of one control reflects state, the way a select shows
  // its value.
  let myDepartment = $derived(
    session.value.kind === 'ready' ? session.value.user.department : '',
  );

  let pinned = $derived(
    apps.filter((a: AppTab) => a.id === 'home' || a.id === 'simulator' || a.id === myDepartment),
  );
  let more = $derived(apps.filter((a: AppTab) => !pinned.some((p: AppTab) => p.id === a.id)));
  /// The active app when it lives under More — so the bar can say so.
  let activeInMore = $derived(more.find((a: AppTab) => a.id === active) ?? null);

  let moreOpen = $state(false);

  // Escape closes the menu. A plain listener rather than a
  // svelte:window tag, which the bundler cannot resolve — see
  // no-svelte-window.test.ts.
  $effect(() => {
    function onKeyDown(e: KeyboardEvent) {
      if (e.key === 'Escape') moreOpen = false;
    }
    window.addEventListener('keydown', onKeyDown);
    return () => window.removeEventListener('keydown', onKeyDown);
  });
</script>

<nav class="perspective-tabs" aria-label="Perspective">
  <span class="perspective-brand">
    <LoopMark size={22} band />
    <span class="perspective-brand-name">{brandName}</span>
    {#if brandSub}<span class="perspective-brand-sub">{brandSub}</span>{/if}
  </span>
  <div class="perspective-tablist">
    {#each pinned as t (t.id)}
      <a
        class="perspective-tab"
        class:active={active === t.id}
        href={t.href}
        aria-current={active === t.id ? 'page' : undefined}
      >{t.label}</a>
    {/each}
    {#if more.length}
      <div class="perspective-more">
        <button
          type="button"
          class="perspective-tab perspective-more-btn"
          class:active={activeInMore !== null}
          aria-expanded={moreOpen}
          aria-haspopup="true"
          aria-current={activeInMore ? 'page' : undefined}
          onclick={() => (moreOpen = !moreOpen)}
        >{activeInMore ? activeInMore.label : 'More'}
          <span aria-hidden="true">▾</span></button>
        {#if moreOpen}
          <!-- Click-away. Sits under the menu, over everything else. -->
          <button
            type="button"
            class="perspective-more-scrim"
            aria-label="Close menu"
            onclick={() => (moreOpen = false)}
          ></button>
          <div class="perspective-more-menu">
            {#each more as t (t.id)}
              <a
                class="perspective-more-item"
                class:perspective-more-item-on={active === t.id}
                href={t.href}
                aria-current={active === t.id ? 'page' : undefined}
              >{t.label}</a>
            {/each}
          </div>
        {/if}
      </div>
    {/if}
  </div>
  <div class="perspective-right">
    <GlobalSearch appKinds={searchAppKinds} />
    <FeedbackControl />
    <SystemTime />
    <SignInControl />
  </div>
</nav>

<style>
  /* An ENAMEL BAND: white words let into night ink, the way Enamel draws
     every header (the fold, "Its parts are Enamel"; backlog 7eb59678
     car 2). Until this car the bar was the white of the page it sat on,
     told apart by one hairline — the one piece of furniture every app
     shares, and the least legible as furniture.

     The one bold move is the tab you are on: it is not tinted or
     underlined but CUT OUT of the band, the page's own white ground
     rising into it, so the bar visibly opens onto the page below. The
     other tabs are the same notch waiting (their hover shows its shape).
     Every control in the bar declares its own on-band colour — nothing
     here inherits the document's ink, which is the defect
     chrome-consistency.mocked.spec.ts was written for. */
  .perspective-tabs {
    position: fixed;
    top: 0;
    left: 0;
    right: 0;
    height: 44px;
    z-index: 60;
    display: flex;
    align-items: stretch;
    gap: 20px;
    background: var(--band);
    color: var(--on-band);
    padding: 0 16px;
  }
  .perspective-more {
    position: relative;
    display: flex;
    align-items: stretch;
  }
  .perspective-more-btn {
    font: inherit;
    border: none;
    cursor: pointer;
    background: none;
  }
  /* Full-viewport click-away, beneath the menu. A button so it is
     reachable and dismissable without a mouse. */
  .perspective-more-scrim {
    position: fixed;
    inset: 0;
    z-index: 1;
    border: none;
    padding: 0;
    background: transparent;
    cursor: default;
  }
  .perspective-more-menu {
    position: absolute;
    top: 100%;
    left: 0;
    z-index: 2;
    min-width: 180px;
    padding: 6px;
    display: flex;
    flex-direction: column;
    background: var(--ink);
    border: 1px solid var(--hairline);
    border-top: 0;
    border-radius: 0 0 var(--radius) var(--radius);
  }
  .perspective-more-item {
    padding: 7px 10px;
    border-radius: 4px;
    font-size: 13px;
    color: var(--static);
    text-decoration: none;
    white-space: nowrap;
  }
  .perspective-more-item:hover {
    background: var(--wash);
    color: var(--fog);
  }
  .perspective-more-item-on {
    color: var(--fog);
    font-weight: 500;
  }

  /* The lockup: loop mark + mono wordmark, per §03. Center-aligned rather
     than baseline now that a mark sits beside the text. */
  .perspective-brand {
    display: flex;
    align-items: center;
    gap: 9px;
    flex: 0 0 auto;
  }
  .perspective-brand-name {
    font-family: var(--font-mono);
    font-size: 14px;
    font-weight: 500;
    text-transform: uppercase;
    letter-spacing: var(--ls-nav);
    color: var(--on-band);
    white-space: nowrap;
  }
  .perspective-brand-sub {
    font-family: var(--font-mono);
    font-size: 10px;
    font-weight: 400;
    text-transform: uppercase;
    letter-spacing: 0.2em;
    /* Was brewery amber. Not SIGNAL either: the cut-out tab already
       answers "where am I", which is the more useful signal of the two. */
    color: var(--on-band-dim);
  }
  .perspective-tablist {
    display: flex;
    align-items: stretch;
  }
  /* Nav is instrument type — §03 assigns NAV to DM Mono, caps and
     letterspaced. */
  /* Every tab is a notch in the band: squared top corners, open at the
     foot, starting 8px below the band's top edge so the ink frames it on
     three sides. */
  .perspective-tab {
    display: flex;
    align-items: center;
    margin-top: 8px;
    padding: 2px 18px 0;
    border-radius: var(--radius) var(--radius) 0 0;
    font-family: var(--font-mono);
    font-size: 12px;
    font-weight: 400;
    text-transform: uppercase;
    letter-spacing: var(--ls-label);
    color: var(--on-band-dim);
    text-decoration: none;
    transition:
      color 0.1s,
      background 0.1s;
  }
  .perspective-tab:hover {
    color: var(--on-band);
    background: var(--band-raised);
  }
  /* Where you are: the page's own ground cut into the band, its word in
     the page's ink. No hue is spent on it — the shape says it. */
  .perspective-tab.active,
  .perspective-tab.active:hover {
    color: var(--fog);
    font-weight: 600;
    background: var(--void);
  }
  .perspective-right {
    margin-left: auto;
    display: flex;
    align-items: center;
    gap: 14px;
    flex: 0 0 auto;
  }

  /* ON A PHONE (design 62de32ae decision 12, car G; the width is
     PHONE_QUERY in ui/phone.ts, pinned by its test). Measured at 390px:
     the bar's content ran to 877px, so the tab you are on, search,
     feedback and sign-in were past the edge of the screen with no way
     to reach them. The band keeps its 44px — every shell offsets below
     it — and scrolls sideways inside itself instead; the wordmark gives
     its room to the tabs, the mark stays. A scroll container clips what
     hangs out of it, so the More menu is pinned under the band rather
     than to its button. */
  @media (max-width: 720px) {
    .perspective-tabs { gap: 12px; overflow-x: auto; overflow-y: hidden; scrollbar-width: none; }
    .perspective-brand-name, .perspective-brand-sub { display: none; }
    .perspective-more-menu { position: fixed; top: 44px; left: 8px; right: 8px; }
  }
</style>
