<script lang="ts">
  // What a visitor gets at `/` instead of somebody else's dashboard.
  //
  // A guest used to land on My Day. Rendered for someone with no
  // assignments that is three empty employee panels — "Nothing in your
  // personal queue", "Nothing waiting on your role's queue", a
  // watchlist that fails to load — under a header reading
  // "audit-readonly · 0.0 years · visitor". David: "I think the Guest
  // landing looks too much like an employee view still. I like the 'My
  // Watchlist', and I think we show that more as job cards moving
  // through stations instead of just a static list. I also think we
  // need a greeting message and some other orientation about what
  // Guests experience wandering Algedonic Ales."
  //
  // So: greet, orient, and keep the one panel that was worth keeping —
  // but as motion. The watchlist for a visitor IS the feedback they
  // sent, and the honest way to show what became of it is to stand
  // each piece at the stop it reached.
  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import { href, navigate } from '../router';
  import { NO_TRACK, placeOnTrack, type FeedbackPacket, type PacketTrack } from './packetTrack';

  type Props = Readonly<{ greeting: string }>;
  let { greeting }: Props = $props();

  let track = $state<PacketTrack>(NO_TRACK);
  /// Non-null when the feedback read FAILED (5xx / network) — the
  /// panel says so instead of silently vanishing (packet 3fba9c35).
  /// A policy refusal (403) keeps the old quiet absence: a guest
  /// whose read scope says no is not looking at an outage.
  let trackFailed = $state<string | null>(null);

  $effect(() => {
    let cancelled = false;
    (async () => {
      try {
        const r = await fetch('/api/jobs?kind=user-feedback&limit=100');
        if (!r.ok) {
          // Read scope says no: no panel, no error. Anything else
          // failing is an outage, not an answer.
          if (!cancelled && r.status !== 401 && r.status !== 403) {
            trackFailed = `HTTP ${r.status}`;
          }
          return;
        }
        const body = await r.json();
        const rows: FeedbackPacket[] = Array.isArray(body) ? body : (body.data ?? []);
        if (!cancelled) {
          track = placeOnTrack(rows);
          trackFailed = null;
        }
      } catch (e) {
        if (!cancelled) trackFailed = e instanceof Error ? e.message : String(e);
      }
    })();
    return () => {
      cancelled = true;
    };
  });

  function go(path: string): void {
    navigate(href(path));
  }
</script>

<div class="theme-exec" style="padding: 0 32px 32px">
  <PageHeader
    eyebrow={greeting}
    title="Welcome to Algedonic Ales"
    subtitle="A working brewery you can walk around in"
    motif="glass"
  />

  <p class="guest-lede">
    Everything here is the real thing rather than a demo of one. The
    beer is real, the orders are real, and the machinery underneath is
    the software we build — so you can open any of it and see how the
    place actually runs.
  </p>

  <!-- Orientation in the order a visitor needs it: what can I do,
       what happens when I do it, what am I looking at. -->
  <section class="guest-tour">
    <button type="button" class="guest-tour-card" onclick={() => go('/shop')}>
      <span class="guest-tour-h">Buy some beer →</span>
      <span class="guest-tour-p">
        Order direct from the brewhouse. Availability is read off the
        warehouse as you look at it — if it says two left, there are
        two.
      </span>
    </button>
    <button type="button" class="guest-tour-card" onclick={() => go('/it')}>
      <span class="guest-tour-h">Watch the work move →</span>
      <span class="guest-tour-p">
        Every order becomes a job packet that walks the same brewhouse,
        warehouse and delivery run a wholesale keg order does. Nothing
        is staged for visitors.
      </span>
    </button>
    <span class="guest-tour-card is-static">
      <span class="guest-tour-h">Tell us something</span>
      <span class="guest-tour-p">
        The FEEDBACK button up in the bar is not a suggestion box. What
        you send opens a job in our IT department — and below is where
        those jobs have got to.
      </span>
    </span>
  </section>

  {#if trackFailed}
    <p class="empty load-failed" role="alert">
      Couldn't load the feedback track — {trackFailed}
    </p>
  {/if}
  {#if track.any}
    <section class="guest-track-wrap">
      <h2 class="guest-track-title">What guests told us, and where it got to</h2>
      <div class="guest-track">
        {#each track.stops as stop (stop.key)}
          <div class="guest-stop">
            <div class="guest-stop-head">
              <span class="guest-stop-dot" class:lit={stop.cards.length > 0}></span>
              <span class="guest-stop-label">{stop.label}</span>
            </div>
            <div class="guest-stop-cards">
              {#each stop.cards as card (card.id)}
                <article class="guest-packet">
                  <span class="guest-packet-about">{card.about}</span>
                  <span class="guest-packet-when">{card.when}</span>
                </article>
              {:else}
                <p class="guest-stop-empty">—</p>
              {/each}
            </div>
          </div>
        {/each}
      </div>
      <p class="guest-track-foot">
        {track.received} pieces of feedback so far, {track.done} of them
        built and shipped{#if track.setAside > 0}, {track.setAside} we
        read and didn't take up{/if}. Every card is a job packet with an
        owner and an audit trail — the same machinery that moves a keg.
      </p>
    </section>
  {/if}
</div>

<style>
  .guest-lede {
    color: var(--fog, #E8ECEF);
    font-size: 15px;
    line-height: 1.65;
    max-width: 64ch;
    margin: 4px 0 22px;
  }

  .guest-tour {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(240px, 1fr));
    gap: 14px;
    margin-bottom: 28px;
  }
  /* Two of the three are doors, so they are buttons; the third is a
     statement about a control that already exists in the chrome, and
     dressing it as a door would be a dead one. */
  .guest-tour-card {
    display: flex;
    flex-direction: column;
    gap: 6px;
    text-align: left;
    border: 1px solid var(--hairline, #2A3138);
    border-radius: 8px;
    padding: 14px 16px;
    background: none;
    color: inherit;
    font: inherit;
    cursor: pointer;
  }
  .guest-tour-card.is-static {
    cursor: default;
  }
  .guest-tour-card:not(.is-static):hover {
    border-color: var(--signal, #29C7B0);
  }
  .guest-tour-h {
    font-size: 15px;
    font-weight: 600;
  }
  .guest-tour-p {
    font-size: 14px;
    line-height: 1.6;
    color: var(--fog, #E8ECEF);
  }

  .guest-track-title {
    font-size: 15px;
    margin: 0 0 14px;
  }
  /* Stops read left to right so the eye reads motion. The row scrolls
     inside its own box rather than widening the page. */
  .guest-track {
    display: grid;
    grid-auto-flow: column;
    grid-auto-columns: minmax(150px, 1fr);
    gap: 10px;
    overflow-x: auto;
    padding-bottom: 6px;
  }
  .guest-stop-head {
    display: flex;
    align-items: center;
    gap: 7px;
    padding-bottom: 8px;
    border-bottom: 1px solid var(--hairline, #2A3138);
    margin-bottom: 10px;
  }
  /* Lit only where something is standing. An all-lit track would claim
     a busyness that is rarely true and always noticed. */
  .guest-stop-dot {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--hairline, #2A3138);
    flex: 0 0 auto;
  }
  .guest-stop-dot.lit {
    background: var(--signal, #29C7B0);
  }
  .guest-stop-label {
    font-size: 12px;
    line-height: 1.3;
    color: var(--fog, #E8ECEF);
  }
  .guest-stop-cards {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .guest-packet {
    border: 1px solid var(--hairline, #2A3138);
    border-left: 2px solid var(--signal, #29C7B0);
    border-radius: 6px;
    padding: 8px 10px;
    display: flex;
    flex-direction: column;
    gap: 3px;
  }
  .guest-packet-about {
    font-size: 12px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .guest-packet-when {
    font-family: var(--font-mono, ui-monospace, monospace);
    font-size: 11px;
    color: var(--static, #7A838C);
  }
  .guest-stop-empty {
    color: var(--static, #7A838C);
    margin: 0;
    font-size: 13px;
  }
  .guest-track-foot {
    color: var(--static, #7A838C);
    font-size: 13px;
    line-height: 1.65;
    margin: 14px 0 0;
    max-width: 70ch;
  }
</style>
