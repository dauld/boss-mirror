<script lang="ts">
  // THE IT WORLD ON A PHONE (design 62de32ae decision 12, car G): a
  // vertical strip map in place of the SVG world, which is not shrunk.
  // One row per region in flow order, grouped under the three thirds;
  // each row the region's state, its KPI and the rail coming into it —
  // what waits there and how fast it crosses. Every row is the same door
  // its territory is: a tap swaps the view to the region's map, at
  // /it/yard/<region>.
  //
  // It reads what the world reads and nothing more — the regions and
  // borders MapPage already holds — and every word is the server's
  // (phone-strip.ts places them; it derives nothing).
  import { navigate } from '@boss/web-kit/nav';
  import type { Borders } from './borders';
  import { countText, floorHref, kpiText, lampOf, stateText, type Regions } from './regions';
  import { railLine, stripGroups, verdictOf } from './phone-strip';

  type Props = Readonly<{
    regions: Regions;
    /** The rails, or null while unread or unreadable — each row then
     *  says its rail in is not read, never that nothing waits there. */
    borders?: Borders | null;
  }>;
  let { regions, borders = null }: Props = $props();

  const groups = $derived(stripGroups(regions, borders));

  function open(e: MouseEvent, href: string): void {
    if (e.metaKey || e.ctrlKey || e.shiftKey || e.button !== 0) return;
    e.preventDefault();
    navigate(href);
  }
</script>

<section class="strip" aria-label="the IT world, one row per region">
  {#each groups as g (g.third)}
    <div class="strip-third" data-third={g.third}>
      <h2 class="strip-third-label">{g.label}</h2>
      <ol class="strip-rows">
        {#each g.rows as row (row.name)}
          {@const r = row.region}
          {@const state = r?.state ?? 'troubled'}
          <li>
            <a
              class="strip-row"
              data-region={row.name}
              data-state={state}
              href={floorHref(row.name)}
              onclick={(e) => open(e, floorHref(row.name))}>
              <span class="strip-head">
                <span class="strip-name">{row.name}</span>
                <span class="strip-state"><span class="lamp {lampOf(state)}"></span>{stateText(r)}</span>
              </span>
              <!-- THE KPI, in the server's words and units (decision
                   9); the count in its unit on an older payload that
                   sends no KPI. -->
              <span class="strip-kpi">{r === undefined ? 'no reading' : kpiText(r) || countText(r)}</span>
              {#each verdictOf(r) as line, i (i)}
                <span class="strip-verdict" class:err={state === 'troubled'}>{line}</span>
              {/each}
              <!-- THE RAIL IN: what waits at this region's border and
                   how fast it crosses (decision 12). -->
              {#if row.rails === null}
                <span class="strip-rail" data-rail="unread"><span class="lamp unknown"></span>rail in: no reading</span>
              {:else if row.rails.length === 0}
                <span class="strip-rail" data-rail="none">no rail in — work arrives from outside the map</span>
              {:else}
                {#each row.rails as b (b.from)}
                  <span class="strip-rail" data-rail="{b.from}→{b.to}" data-state={b.state}
                    ><span class="lamp {lampOf(b.state)}"></span>{railLine(b)}</span>
                {/each}
              {/if}
            </a>
          </li>
        {/each}
      </ol>
    </div>
  {/each}
</section>

<style>
  /* The map's own --map-* tokens, no fallback (42f66fb3,
     map-palette.test.ts), in the world's grammar: mono names in caps,
     the state as a lamp, the unmeasured muted. */
  .strip { margin-top: var(--s3); }
  .strip-third + .strip-third { margin-top: var(--s4); }
  .strip-third-label {
    margin: 0 0 var(--s2);
    font-family: var(--font-mono);
    font-size: 11px;
    font-weight: 500;
    letter-spacing: var(--ls-nav);
    text-transform: uppercase;
    color: var(--map-muted);
  }
  .strip-rows { list-style: none; margin: 0; padding: 0; }
  .strip-rows li + li { margin-top: var(--s2); }
  .strip-row {
    display: flex;
    flex-direction: column;
    gap: 3px;
    padding: 10px 12px;
    background: var(--map-bg);
    border: 1px solid var(--map-rule);
    border-left: 3px solid var(--map-ok-edge);
    color: var(--map-ink);
    text-decoration: none;
    min-width: 0;
  }
  .strip-row[data-state='attention'] { border-left-color: var(--map-warn-edge); }
  .strip-row[data-state='troubled'] { border-left-color: var(--map-bad-edge); }
  .strip-head { display: flex; justify-content: space-between; align-items: baseline; gap: var(--s2); }
  .strip-name {
    font-family: var(--font-mono);
    font-size: 13px;
    font-weight: 600;
    letter-spacing: var(--ls-nav);
    text-transform: uppercase;
  }
  .strip-state { font-family: var(--font-mono); font-size: 11px; color: var(--map-muted); white-space: nowrap; }
  .strip-row[data-state='attention'] .strip-state { color: var(--map-warn-ink); }
  .strip-row[data-state='troubled'] .strip-state { color: var(--map-bad-ink); }
  .strip-kpi { font-size: 14px; overflow-wrap: anywhere; }
  .strip-verdict { font-size: 12px; color: var(--map-warn-ink); overflow-wrap: anywhere; }
  .strip-verdict.err { color: var(--map-bad-ink); }
  .strip-rail { font-family: var(--font-mono); font-size: 11px; color: var(--map-muted); overflow-wrap: anywhere; }
  .lamp {
    display: inline-block;
    width: 7px;
    height: 7px;
    margin-right: 6px;
    border-radius: 50%;
    border: 1px dashed var(--map-muted);
    vertical-align: 1px;
  }
  .lamp.ok { background: var(--map-ok-edge); border-color: var(--map-ok-edge); border-style: solid; }
  .lamp.warn { background: var(--map-warn-edge); border-color: var(--map-warn-edge); border-style: solid; }
  .lamp.err { background: var(--map-bad-edge); border-color: var(--map-bad-edge); border-style: solid; }
</style>
