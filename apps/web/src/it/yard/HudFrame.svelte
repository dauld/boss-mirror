<script lang="ts">
  // THE HUD FRAME (design 00774ca8, approved by David 2026-09-24; car F
  // of design 62de32ae). A band above the map, three rows in normal
  // type — one per third of the operator surface, in the payload's
  // order — and one machine cell to their right. Every figure is a
  // field of the one /api/yard/regions read (`boss orient` prints the
  // same block); hud.ts turns the fields into the four pictures and
  // this file only draws them. Nothing here adds anything up.
  //
  // FIXED (decision 6): the frame shows the same cells in the same
  // places on /it and on every /it/yard/<region> — the HUD answers about
  // the whole system, so it does not follow the zoom. Below the rows the
  // contextual strip keeps its height reserved whether or not anything is
  // selected, so the map under it never jumps.
  //
  // THE MARKS ARE ENAMEL PLATES (decision 7, backlog 7eb59678): the
  // shared `.plate .plate-troubled` for a stuck count above zero and a
  // failed machine; balance wears no colour until a band is declared for
  // it; no lamp for a row as a whole, which would be a worst-of that
  // hides which figure moved. Colours are the map's --map-* tokens only
  // (map-palette.test.ts reads this file).
  import { onMount, type Snippet } from 'svelte';
  import { navigate } from '@boss/web-kit/nav';
  import type { Remote } from '../../data/remote';
  import type { Regions } from './regions';
  import { hudOf, type Figure, type LastGood } from './hud';

  type Props = Readonly<{
    /** The latest regions read, landed at `readAt`. */
    read: Remote<Regions>;
    readAt: number | null;
    /** The newest read that succeeded: its time for the failure header,
     *  its row names for the frame's shape — never its values. */
    lastGood: LastGood | null;
    /** What the contextual strip shows for the current selection. */
    strip?: Snippet;
  }>;
  let { read, readAt, lastGood, strip }: Props = $props();

  // The read's age ticks on its own clock — the page polls every 10s,
  // and "read 4s ago" that never moves would itself be a stale figure.
  let now = $state(Date.now());
  onMount(() => {
    const t = setInterval(() => (now = Date.now()), 1000);
    return () => clearInterval(t);
  });

  const hud = $derived(hudOf(read, readAt, lastGood, now));

  function open(e: MouseEvent, href: string): void {
    if (e.metaKey || e.ctrlKey || e.shiftKey || e.button !== 0) return;
    e.preventDefault();
    navigate(href);
  }
</script>

{#snippet fig(f: Figure)}
  {#if f.kind === 'zero'}
    <span class="fig" data-fig="zero">0</span>
  {:else if f.kind === 'value'}
    <span class="fig" class:plate={f.troubled} class:plate-troubled={f.troubled} data-fig="value">{f.text}</span>
  {:else if f.kind === 'floor'}
    <span class="fig" data-fig="floor" title={f.why}
      ><span class:plate={f.troubled} class:plate-troubled={f.troubled}>{f.text}</span><span class="q-band">?</span></span>
  {:else}
    <span class="fig q-housing" data-fig="unread" title={f.why}>?</span>
  {/if}
{/snippet}

<!-- Every composed line is a flex row of separate tokens with a gap,
     so no word and no figure depends on the whitespace between a text
     node and a rendered snippet. -->
<section class="hud" data-hud data-read={hud.read} aria-label="The whole system">
  <header class="hud-head">
    <span class="hud-title">The whole system</span>
    <span class="hud-age" class:failed={hud.read === 'failed'}>{hud.header}</span>
  </header>
  <div class="hud-body">
    <div class="hud-rows">
      {#each hud.rows as row, i (i)}
        <div class="hud-row" data-third={row.third}>
          <div class="hud-label" title={row.regions.join(' · ')}>{row.label}</div>
          <div class="hud-cell" data-cell="balance" title={row.means}>
            <div class="line">
              <span class="hud-key">balance</span>
              {@render fig(row.net)}
            </div>
            <div class="line hud-rates">
              {@render fig(row.into)}<span>in</span><span class="dot">·</span>{@render fig(row.out)}<span
                >out{row.unit === '' ? '' : ` ${row.unit}`}/day</span>
            </div>
          </div>
          <div class="hud-cell" data-cell="stuck">
            <div class="line">
              <span class="hud-key">stuck</span>
              {@render fig(row.stuck)}
              {#if row.oldest !== null}<span class="hud-sub">{row.oldest}</span>{/if}
              <span class="dot">·</span>
              <span class="hud-key">waiting</span>
              {@render fig(row.waiting)}
            </div>
          </div>
          <div class="hud-cell" data-cell={row.arrivals === null ? undefined : 'arrivals'}>
            {#if row.arrivals !== null}
              <!-- Decision 4's one exception: the arrivals territory's
                   own trend field, rendered twice, cannot drift. -->
              <div class="line">
                <span class="hud-key">arrivals</span>
                {@render fig(row.arrivals.current)}<span class="hud-sub">/day, was</span>{@render fig(
                  row.arrivals.previous,
                )}
              </div>
            {/if}
          </div>
        </div>
      {/each}
    </div>
    <div class="hud-machines" data-machines>
      <div class="hud-key">machines</div>
      <div class="line hud-machine-line">
        {@render fig(hud.machines.failed)}<span>failed</span><span class="dot">·</span>{@render fig(
          hud.machines.unjudged,
        )}<span>unjudged of</span>{@render fig(hud.machines.total)}
      </div>
      {#if hud.machines.listed.length > 0}
        <ul class="hud-listed">
          {#each hud.machines.listed as m (`${m.region}:${m.id}`)}
            <li>
              <a href={`/it/yard/${m.region}`} title={m.why} data-machine={m.id}
                onclick={(e) => open(e, `/it/yard/${m.region}`)}
                >{`${m.state === 'failed' ? 'failed' : 'unjudged'} · ${m.region} · ${m.name}`}</a>
            </li>
          {/each}
        </ul>
      {/if}
    </div>
  </div>
  <div class="hud-strip" data-strip>
    {#if strip}{@render strip()}{/if}
  </div>
</section>

<style>
  .hud { border: 1px solid var(--map-rule); border-radius: var(--radius); margin: 0 0 16px;
    background: var(--map-surface); color: var(--map-ink); font-size: 13px; }
  .hud-head { display: flex; justify-content: space-between; gap: 4px 12px; flex-wrap: wrap;
    padding: 8px 12px; border-bottom: 1px solid var(--map-rule); }
  .hud-title { font-family: var(--font-mono); font-size: 11px; letter-spacing: var(--ls-nav);
    text-transform: uppercase; color: var(--map-muted); }
  .hud-age { font-family: var(--font-mono); font-size: 11px; color: var(--map-muted); }
  .hud-age.failed { color: var(--map-bad-ink); }
  .hud-body { display: grid; grid-template-columns: minmax(0, 1fr) minmax(200px, 260px); }
  .hud-rows { min-width: 0; }
  .hud-row { display: grid; grid-template-columns: 140px minmax(0, 1.15fr) minmax(0, 1.25fr) minmax(0, 0.9fr);
    align-items: start; gap: 4px 16px; padding: 7px 12px; }
  .hud-row + .hud-row { border-top: 1px solid var(--map-rule); }
  .hud-label { font-weight: 600; }
  .hud-cell { min-width: 0; }
  .line { display: flex; flex-wrap: wrap; align-items: baseline; column-gap: 5px; }
  .hud-key { font-family: var(--font-mono); font-size: 10.5px; letter-spacing: var(--ls-nav);
    text-transform: uppercase; color: var(--map-muted); }
  .hud-rates, .hud-sub { font-size: 11.5px; color: var(--map-muted); }
  .hud-rates { margin-top: 1px; }
  .dot { color: var(--map-muted); }
  .hud-machines { border-left: 1px solid var(--map-rule); padding: 7px 12px; min-width: 0; }
  .hud-machine-line { margin-top: 2px; }
  .hud-listed { list-style: none; margin: 6px 0 0; padding: 0; font-size: 11.5px; }
  .hud-listed li { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .hud-listed a { color: var(--map-link); text-decoration: none; }
  .hud-listed a:hover { text-decoration: underline; }
  /* The contextual strip: empty until something is selected, and its
     height reserved either way (decision 6). */
  .hud-strip { min-height: 28px; border-top: 1px solid var(--map-rule); padding: 4px 12px;
    font-size: 12px; color: var(--map-muted); }

  /* THE FOUR PICTURES (decision 5). A zero and a value are plain; a
     floor's uncounted part and an unread figure are the map's unknown:
     a broken outline, muted — never a zero, never blank. */
  .fig { font-variant-numeric: tabular-nums; }
  .q-housing, .q-band { display: inline-block; min-width: 1.4em; padding: 0 4px; text-align: center;
    border: 1px dashed var(--map-muted); border-radius: var(--radius); color: var(--map-muted);
    font-family: var(--font-mono); font-size: 11px; line-height: 15px; }
  .q-band { margin-left: 3px; }

  /* Phone: the machine cell stands under the rows and each row stacks
     its cells, so nothing scrolls sideways (car G folds the rows into
     the strip map's group headers). */
  @media (max-width: 720px) {
    .hud-body { grid-template-columns: minmax(0, 1fr); }
    .hud-machines { border-left: none; border-top: 1px solid var(--map-rule); }
    .hud-row { grid-template-columns: minmax(0, 1fr); gap: 2px; }
  }
</style>
