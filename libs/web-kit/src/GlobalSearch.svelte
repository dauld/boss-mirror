<script lang="ts">
  // Global search — the chrome bar's third element.
  //
  // Q4, folded into docs/architecture-decisions.md §Search when the
  // source doc was flattened away: a dropdown that prioritises the
  // app you are in, with a final link through to the full results
  // surface in Home. Not chrome-or-Home; chrome AND Home, because a
  // preview and a result set are different affordances and forcing one
  // to be the other makes both worse.
  //
  // `appKinds` is a ranking hint the host passes down (apps/web knows
  // which Subject kinds belong to which app; web-kit does not, and
  // should not). It never filters — a global box that hides results
  // because you are in the wrong app is not a global box.
  import { href } from './nav';

  let { appKinds = [] as ReadonlyArray<string> } = $props<{
    appKinds?: ReadonlyArray<string>;
  }>();

  type Row = {
    ref_kind: 'subject' | 'job' | 'event';
    ref_id: string;
    subject_kind: string | null;
    subject_id: string | null;
    title: string;
    body: string;
  };
  type SubjectHit = {
    subject_kind: string;
    subject_id: string;
    title: string;
    jobs: Row[];
    events: Row[];
    event_count: number;
  };
  type Results = {
    query: string;
    subjects: SubjectHit[];
    jobs: Row[];
    events: Row[];
  };

  let q = $state('');
  let open = $state(false);
  let loading = $state(false);
  let results = $state<Results | null>(null);
  let error = $state<string | null>(null);
  let seq = 0;

  async function run(term: string): Promise<void> {
    const mine = ++seq;
    if (!term.trim()) {
      results = null;
      loading = false;
      return;
    }
    loading = true;
    error = null;
    try {
      const params = new URLSearchParams({ q: term });
      if (appKinds.length) params.set('app_kinds', appKinds.join(','));
      const r = await fetch(`/api/search?${params}`);
      if (!r.ok) throw new Error(`HTTP ${r.status}`);
      const body = (await r.json()) as Results;
      // Drop a response that a newer keystroke has already superseded,
      // otherwise a slow early query can overwrite a fast later one.
      if (mine !== seq) return;
      results = body;
    } catch (e) {
      if (mine !== seq) return;
      error = e instanceof Error ? e.message : String(e);
    } finally {
      if (mine === seq) loading = false;
    }
  }

  let debounce: ReturnType<typeof setTimeout> | undefined;
  function onInput(e: Event): void {
    q = (e.target as HTMLInputElement).value;
    open = true;
    clearTimeout(debounce);
    debounce = setTimeout(() => void run(q), 180);
  }

  function pathFor(kind: string, id: string): string {
    // Subject kinds the SPA has a detail route for. Anything else
    // falls back to a jobs-filtered view, which is always meaningful:
    // every Subject can at least answer "what work is about me".
    const direct: Record<string, string> = {
      account: `/ux/accounts/${id}`,
      vendor: `/ux/vendors/${id}`,
      employee: `/ux/people/${id}`,
      product: `/ux/products/${id}`,
      asset: `/ux/assets/${id}`,
    };
    return (
      direct[kind] ??
      `/ux/jobs?subject_kind=${encodeURIComponent(kind)}&subject_id=${encodeURIComponent(id)}`
    );
  }

  function close(): void {
    open = false;
  }

  function onKey(e: KeyboardEvent): void {
    if (e.key === 'Escape') close();
    if (e.key === 'Enter' && q.trim()) {
      window.location.href = href(`/ux/search?q=${encodeURIComponent(q)}`);
    }
  }

  let hasAny = $derived(
    !!results &&
      (results.subjects.length > 0 ||
        results.jobs.length > 0 ||
        results.events.length > 0),
  );
</script>

<div class="gs">
  <input
    class="gs-input"
    type="search"
    placeholder="Search"
    value={q}
    oninput={onInput}
    onfocus={() => (open = true)}
    onkeydown={onKey}
    aria-label="Global search"
  />

  {#if open && q.trim()}
    <!-- Click-away. A transparent full-viewport layer is the smallest
         thing that closes on any outside click without a document
         listener that outlives the component. -->
    <button class="gs-scrim" onclick={close} tabindex="-1" aria-hidden="true"></button>
    <div class="gs-panel">
      {#if loading && !results}
        <div class="gs-msg">Searching…</div>
      {:else if error}
        <div class="gs-msg gs-err">{error}</div>
      {:else if !hasAny}
        <div class="gs-msg">No matches for “{q}”.</div>
      {:else if results}
        {#each results.subjects.slice(0, 5) as s (s.subject_kind + s.subject_id)}
          <a class="gs-row" href={href(pathFor(s.subject_kind, s.subject_id))} onclick={close}>
            <span class="gs-kind">{s.subject_kind}</span>
            <span class="gs-title">{s.title}</span>
            <!-- The unified layer, shown rather than claimed: this is
                 one Subject with its work and its history attached, not
                 three lists that mention it. -->
            <span class="gs-sub">
              {s.jobs.length}
              {s.jobs.length === 1 ? 'job' : 'jobs'} · {s.event_count} events
            </span>
          </a>
        {/each}

        {#each results.jobs.slice(0, 3) as j (j.ref_id)}
          <a class="gs-row" href={href(`/ux/jobs/${j.ref_id}`)} onclick={close}>
            <span class="gs-kind">job</span>
            <span class="gs-title">{j.title}</span>
            <span class="gs-sub">{j.body}</span>
          </a>
        {/each}

        {#each results.events.slice(0, 3) as e (e.ref_id)}
          <div class="gs-row gs-row-static">
            <span class="gs-kind">event</span>
            <span class="gs-title">{e.title}</span>
            <span class="gs-sub">{e.subject_kind ?? ''} {e.subject_id ?? ''}</span>
          </div>
        {/each}
      {/if}

      <a class="gs-all" href={href(`/ux/search?q=${encodeURIComponent(q)}`)} onclick={close}>
        All results for “{q}” →
      </a>
    </div>
  {/if}
</div>

<style>
  .gs {
    position: relative;
    flex: 0 1 260px;
    min-width: 140px;
  }
  .gs-input {
    width: 100%;
    box-sizing: border-box;
    font: inherit;
    font-size: 13px;
    padding: 4px 10px;
    /* A white field let into the enamel band (backlog 7eb59678 car 2):
       squared to a field's 3px, and focused with the amber ring — the
       action blue measures 3.1:1 against the band, too faint to find. */
    border-radius: var(--radius-field);
    border: 1px solid var(--border);
    background: var(--bg);
    color: var(--text);
  }
  .gs-input:focus {
    outline: 2px solid var(--focus);
    outline-offset: 1px;
    background: var(--card);
  }
  .gs-scrim {
    position: fixed;
    inset: 0;
    background: none;
    border: none;
    padding: 0;
    cursor: default;
    z-index: 40;
  }
  .gs-panel {
    position: absolute;
    top: calc(100% + 6px);
    right: 0;
    width: min(460px, 92vw);
    max-height: 70vh;
    overflow-y: auto;
    z-index: 41;
    background: var(--card);
    border: 1px solid var(--border);
    border-radius: 8px;
    padding: 4px;
    text-align: left;
  }
  /* On a phone the chrome bar scrolls sideways and so clips anything
     hanging out of it (PerspectiveTabs, car G of design 62de32ae): the
     results pin under the band, the screen's width less a margin. */
  @media (max-width: 720px) {
    .gs-panel { position: fixed; top: 50px; left: 8px; right: 8px; width: auto; }
  }
  .gs-row {
    display: grid;
    grid-template-columns: 74px 1fr auto;
    gap: 8px;
    align-items: baseline;
    padding: 7px 10px;
    border-radius: 5px;
    text-decoration: none;
    color: var(--text);
  }
  .gs-row:hover {
    background: var(--bg);
  }
  .gs-row-static:hover {
    background: none;
  }
  .gs-kind {
    font-size: 10px;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--text-dim);
  }
  .gs-title {
    font-size: 13px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .gs-sub {
    font-size: 11px;
    color: var(--text-dim);
    white-space: nowrap;
  }
  .gs-msg {
    padding: 14px 10px;
    font-size: 13px;
    color: var(--text-dim);
  }
  .gs-err {
    color: var(--err);
  }
  .gs-all {
    display: block;
    margin-top: 4px;
    padding: 8px 10px;
    border-top: 1px solid var(--border);
    font-size: 12px;
    color: var(--accent);
    text-decoration: none;
  }
  .gs-all:hover {
    background: var(--bg);
  }
</style>
