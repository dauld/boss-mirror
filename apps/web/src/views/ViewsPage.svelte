<script lang="ts">
  // Home → Views. The personal rung of the extensibility ladder.
  //
  // Below "author a Workflow" there used to be nothing: an operator who
  // wanted to look at the information a different way could ask for a
  // frontend change or keep a spreadsheet. This is the surface that
  // ends that, and the spreadsheet is what it is competing with — so
  // making a View has to cost less than opening one.
  //
  // A View holds a query and a layout, never rows. Its content comes
  // from the same projections every other surface reads, which is why
  // two people running the same View see the same numbers.
  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import Section from '@boss/web-kit/ui/Section.svelte';
  import { session } from '@boss/web-kit/session/session.svelte';
  import {
    SOURCE_FIELDS,
    type View,
    type ViewLayout,
    type ViewResults,
    type ViewSource,
    type Visibility,
  } from './types';

  let views = $state<ReadonlyArray<View>>([]);
  let loading = $state(true);
  let error = $state<string | null>(null);

  // Results are keyed by view id rather than held on a "selected"
  // view: running two Views and comparing them is the obvious next
  // thing an operator does, and a single slot would forbid it.
  let results = $state<Record<string, ViewResults | undefined>>({});
  let running = $state<Record<string, boolean>>({});
  let rowErrors = $state<Record<string, string | undefined>>({});

  // Draft state for the composer.
  let draftTitle = $state('');
  let draftSource = $state<ViewSource>('jobs');
  let draftFilter = $state('');
  let draftColumns = $state<string[]>([]);
  let draftLayout = $state<ViewLayout>('table');
  let draftVisibility = $state<Visibility>('private');
  let saving = $state(false);
  let saveError = $state<string | null>(null);

  /// Mirrors query.rs SCAN_CEILING — shown, not enforced, here.
  const SCAN_CEILING_LABEL = '5,000';
  /// Mirrors EVENT_PUSHABLE in query.rs.
  /// Mirrors EVENT_PUSHABLE / STEP_PUSHABLE in query.rs.
  const PUSHABLE_BY_SOURCE: Readonly<Record<string, string>> = {
    events: 'kind, source, subject_kind, subject_id, or a timestamp range',
    steps: 'status, kind or assignee_id',
    jobs: 'kind, status, owner_id, subject_kind, subject_id, priority, or a created_at range',
    subjects: 'kind, id, label, or a created_at range',
  };

  let viewerId = $derived(session.value.kind === 'ready' ? session.value.user.id : '');
  let availableFields = $derived(SOURCE_FIELDS[draftSource]);

  async function load(): Promise<void> {
    if (!viewerId) return;
    loading = true;
    error = null;
    try {
      // No viewer_id: identity travels in the request the
      // gateway stamps. Sending it as a param let any caller list
      // any user's private Views by naming them.
      const r = await fetch('/api/views');
      if (!r.ok) throw new Error(`views: HTTP ${r.status}`);
      views = (await r.json()) as View[];
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      loading = false;
    }
  }

  async function run(v: View): Promise<void> {
    running = { ...running, [v.id]: true };
    rowErrors = { ...rowErrors, [v.id]: undefined };
    try {
      const r = await fetch(`/api/views/${v.id}/results?limit=100`);
      if (!r.ok) throw new Error(`HTTP ${r.status}: ${await r.text()}`);
      results = { ...results, [v.id]: (await r.json()) as ViewResults };
    } catch (e) {
      rowErrors = { ...rowErrors, [v.id]: e instanceof Error ? e.message : String(e) };
    } finally {
      running = { ...running, [v.id]: false };
    }
  }

  async function create(): Promise<void> {
    saving = true;
    saveError = null;
    try {
      const r = await fetch('/api/views', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          // owner_id is not on the wire — the server takes it from
          // the authenticated caller.
          title: draftTitle.trim(),
          source: draftSource,
          filter: draftFilter.trim(),
          columns: draftColumns,
          layout: draftLayout,
          visibility: draftVisibility,
        }),
      });
      // 422 carries the filter parse error. Showing it against the
      // form is the whole reason the API distinguishes it from a 500 —
      // the author can fix a typo without guessing.
      if (!r.ok) throw new Error(await r.text());
      draftTitle = '';
      draftFilter = '';
      draftColumns = [];
      await load();
    } catch (e) {
      saveError = e instanceof Error ? e.message : String(e);
    } finally {
      saving = false;
    }
  }

  async function remove(v: View): Promise<void> {
    try {
      const r = await fetch(`/api/views/${v.id}`, { method: 'DELETE' });
      if (!r.ok) throw new Error(`HTTP ${r.status}`);
      await load();
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  }

  function toggleColumn(f: string): void {
    draftColumns = draftColumns.includes(f)
      ? draftColumns.filter((c) => c !== f)
      : [...draftColumns, f];
  }

  /// Column order for a result table: what the View asked for, or the
  /// keys of the first row when it asked for everything.
  function columnsOf(v: View, res: ViewResults): ReadonlyArray<string> {
    if (v.columns.length > 0) return v.columns;
    return res.rows.length > 0 ? Object.keys(res.rows[0]!) : [];
  }

  function cell(value: unknown): string {
    if (value === null || value === undefined) return '—';
    if (typeof value === 'object') return JSON.stringify(value);
    return String(value);
  }

  // One trigger, not two. `onMount(load)` plus an effect that also
  // called load() fired twice on any mount where the session was
  // already resolved, racing two identical requests. The effect alone
  // covers both cases: it runs once on mount and again if the session
  // resolves later.
  let loadedFor = $state<string | null>(null);
  $effect(() => {
    const id = viewerId;
    if (id && loadedFor !== id) {
      loadedFor = id;
      void load();
    }
  });
</script>

<PageHeader
  title="Views"
  subtitle="Your own compositions over the information layer — a query and a layout, never a copy of the data."
/>

<Section title="New view" wide>
  <div class="v-form">
    <label class="v-field">
      <span>Title</span>
      <input bind:value={draftTitle} placeholder="Open wholesale orders" />
    </label>

    <label class="v-field">
      <span>Source</span>
      <select bind:value={draftSource}>
        <option value="jobs">Jobs — the work</option>
        <option value="steps">Steps — what the work is made of</option>
        <option value="subjects">Subjects — the things work is about</option>
        <option value="events">Events — what happened</option>
      </select>
    </label>

    <label class="v-field v-field-wide">
      <span>Filter <em>optional</em></span>
      <input
        class="mono"
        bind:value={draftFilter}
        placeholder={'status = "open" AND kind = "wholesale-keg-order"'}
      />
    </label>

    <div class="v-field v-field-wide">
      <span>Columns <em>none selected shows everything</em></span>
      <div class="v-chips">
        {#each availableFields as f (f)}
          <button
            type="button"
            class="v-chip"
            class:v-chip-on={draftColumns.includes(f)}
            onclick={() => toggleColumn(f)}
          >
            {f}
          </button>
        {/each}
      </div>
    </div>

    <label class="v-field">
      <span>Layout</span>
      <select bind:value={draftLayout}>
        <option value="table">Table</option>
        <option value="list">List</option>
        <option value="count">Count</option>
      </select>
    </label>

    <label class="v-field">
      <span>Visibility</span>
      <select bind:value={draftVisibility}>
        <option value="private">Private — only me</option>
        <option value="shared">Shared — anyone can open it</option>
      </select>
    </label>

    <div class="v-actions">
      <button
        class="btn"
        type="button"
        disabled={saving || draftTitle.trim().length === 0 || !viewerId}
        onclick={create}
      >
        {saving ? 'Saving…' : 'Save view'}
      </button>
      {#if saveError}
        <span class="v-err">{saveError}</span>
      {/if}
    </div>
  </div>
</Section>

{#if loading}
  <p class="v-msg">Loading views…</p>
{:else if error}
  <p class="v-msg v-err">{error}</p>
{:else if views.length === 0}
  <p class="v-msg">
    No views yet. A view is a saved question — pick a source, describe what you
    want to see, and it stays answerable.
  </p>
{:else}
  {#each views as v (v.id)}
    {@const res = results[v.id]}
    <Section title={v.title} wide>
      <div class="v-meta">
        <span class="v-tag">{v.source}</span>
        <span class="v-tag">{v.layout}</span>
        <span class="v-tag" class:v-tag-shared={v.visibility === 'shared'}>
          {v.visibility}
        </span>
        {#if v.owner_id !== viewerId}
          <span class="v-tag">shared by {v.owner_id}</span>
        {/if}
        {#if v.filter}
          <code class="v-filter">{v.filter}</code>
        {/if}
        <span class="v-spacer"></span>
        <button class="btn" type="button" onclick={() => run(v)} disabled={running[v.id]}>
          {running[v.id] ? 'Running…' : 'Run'}
        </button>
        {#if v.owner_id === viewerId}
          <button class="v-del" type="button" onclick={() => remove(v)}>Delete</button>
        {/if}
      </div>

      {#if rowErrors[v.id]}
        <p class="v-msg v-err">{rowErrors[v.id]}</p>
      {:else if res}
        <p class="v-count">
          {res.matched}
          {res.matched === 1 ? 'match' : 'matches'}
          {#if res.truncated && res.pushed_down === 0 && v.filter}
            <!-- The weak case: nothing in the filter could be answered
                 by the database, so this counted only the newest rows.
                 A match older than that window is simply absent, and
                 "0 matches" here does not mean none exist. -->
            <strong class="v-trunc">
              — this filter could not be narrowed in the database, so only the
              newest {SCAN_CEILING_LABEL} events were examined. Older matches are
              not counted. Filtering on {PUSHABLE_BY_SOURCE[v.source] ?? 'a different field'} narrows it.
            </strong>
          {:else if res.truncated}
            <strong class="v-trunc">
              — more than this matched; the count is a floor, not a total
            </strong>
          {/if}
        </p>

        {#if v.layout !== 'count' && res.rows.length > 0}
          {#if v.layout === 'table'}
            <div class="v-scroll">
              <table class="v-table">
                <thead>
                  <tr>
                    {#each columnsOf(v, res) as c (c)}<th>{c}</th>{/each}
                  </tr>
                </thead>
                <tbody>
                  {#each res.rows as row, i (i)}
                    <tr>
                      {#each columnsOf(v, res) as c (c)}<td>{cell(row[c])}</td>{/each}
                    </tr>
                  {/each}
                </tbody>
              </table>
            </div>
          {:else}
            <ul class="v-list">
              {#each res.rows as row, i (i)}
                <li>{columnsOf(v, res).map((c) => cell(row[c])).join(' · ')}</li>
              {/each}
            </ul>
          {/if}
        {/if}
      {/if}
    </Section>
  {/each}
{/if}

<style>
  .v-form {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 12px 16px;
    max-width: 900px;
  }
  .v-field {
    display: flex;
    flex-direction: column;
    gap: 4px;
    font-size: 12px;
    color: var(--text-dim);
  }
  .v-field-wide {
    grid-column: 1 / -1;
  }
  .v-field em {
    font-style: normal;
    color: var(--static);
  }
  .v-field input,
  .v-field select {
    padding: 6px;
    font-size: 13px;
    font: inherit;
    font-size: 13px;
  }
  .mono {
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
  }
  .v-chips {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }
  .v-chip {
    border: 1px solid var(--border);
    background: var(--card);
    border-radius: 999px;
    padding: 3px 10px;
    font-size: 12px;
    cursor: pointer;
  }
  .v-chip-on {
    background: var(--band);
    color: var(--on-band);
    border-color: var(--border-strong);
  }
  .v-actions {
    grid-column: 1 / -1;
    display: flex;
    align-items: center;
    gap: 12px;
  }
  .v-meta {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-wrap: wrap;
    margin-bottom: 10px;
  }
  .v-spacer {
    flex: 1 1 auto;
  }
  .v-tag {
    font-size: 11px;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--text-dim);
    border: 1px solid var(--border);
    border-radius: 4px;
    padding: 1px 6px;
  }
  .v-tag-shared {
    border-color: var(--clear);
    color: var(--ok);
  }
  .v-filter {
    font-size: 12px;
    background: var(--bg);
    padding: 2px 6px;
    border-radius: 4px;
  }
  .v-del {
    background: none;
    border: none;
    color: var(--err);
    font: inherit;
    font-size: 12px;
    cursor: pointer;
  }
  .v-count {
    font-size: 13px;
    margin: 0 0 8px;
  }
  .v-trunc {
    color: var(--warn);
    font-weight: 600;
  }
  .v-scroll {
    overflow-x: auto;
  }
  .v-table {
    border-collapse: collapse;
    font-size: 13px;
    width: 100%;
  }
  .v-table th,
  .v-table td {
    border-bottom: 1px solid var(--border);
    padding: 5px 10px;
    text-align: left;
    white-space: nowrap;
  }
  .v-table th {
    font-size: 11px;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--text-dim);
  }
  .v-list {
    margin: 0;
    padding-left: 18px;
    font-size: 13px;
  }
  .v-msg {
    color: var(--text-dim);
    font-size: 14px;
  }
  .v-err {
    color: var(--err);
  }
</style>
