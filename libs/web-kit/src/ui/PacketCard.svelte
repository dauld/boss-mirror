<script lang="ts">
  // The job-packet card (David's call, 2026-08-12; feedback d69033dd):
  // the one visual for a packet anywhere a queue renders. Protocol
  // names the rail color, tags ride as chips, and a simulated packet
  // wears a dashed border + SIM chip so test traffic can never pass
  // for real work. Promoted here from the train yard so every queue
  // lens draws the same card. Pure presentation plus one affordance —
  // every fact comes off the card data, and double-click (or Enter
  // when focused) opens the packet. What "opens" means belongs to the
  // surface: a lens that renders PacketModal passes `onOpen` and gets
  // the condensed panel without losing the queue it was reading
  // (David, fc67bed2); a lens that does not keeps navigating to the
  // job page.
  import { navigate } from '../nav';
  import { entityHref } from './entity-href';
  import { protocolHue, type PacketCardData } from './packet-card';

  type Props = Readonly<{
    card: PacketCardData;
    size?: 'dock' | 'consist';
    /** What opening the packet means on this surface. A lens that can
     *  show the condensed panel (PacketModal) passes it here; without
     *  one the card keeps navigating to the job page, so surfaces that
     *  have not adopted the modal behave exactly as before. */
    onOpen?: (id: string) => void;
    /** Opt-in dismiss affordance. A lens that lets the reader take a
     *  packet off a personal list (the watchlist) passes it, and the
     *  card grows a small × that calls it with the packet id. Absent
     *  everywhere else, so no other queue surface sprouts a control it
     *  has no meaning for — the card stays a pure read wherever a lens
     *  does not opt in. */
    onDismiss?: (id: string) => void;
  }>;
  let { card, size = 'dock', onOpen, onDismiss }: Props = $props();

  const hue = $derived(protocolHue(card.kind));
  const shownTags = $derived(
    card.tags.filter(t => !['sim', 'simulated', 'synthetic'].includes(t.toLowerCase())).slice(0, 3),
  );

  function open(): void {
    if (onOpen) {
      onOpen(card.id);
      return;
    }
    navigate(entityHref('job', card.id));
  }
  function onKeydown(e: KeyboardEvent): void {
    if (e.key === 'Enter') {
      e.preventDefault();
      open();
    }
  }
  // The × is a real button nested in the card. Its click, double-click
  // and Enter/Space must not also reach the card's open() — dismissing
  // a packet is the opposite of opening it — so each is stopped from
  // bubbling to the card handlers above.
  function dismiss(e: MouseEvent): void {
    e.stopPropagation();
    onDismiss?.(card.id);
  }
  function onDismissKeydown(e: KeyboardEvent): void {
    if (e.key === 'Enter' || e.key === ' ') {
      e.stopPropagation();
    }
  }
</script>

<!-- A generic div, not <article>: the card is interactive (role=link
     + tabindex), and a11y rules bar interactive roles on sectioning
     elements. -->
<div
  class="packet"
  class:compact={size === 'consist'}
  class:sim={card.sim}
  style="--pk: {hue}"
  title={onOpen ? `${card.title} — double-click for the packet` : `${card.title} — double-click to open the job`}
  role="link"
  tabindex="0"
  ondblclick={open}
  onkeydown={onKeydown}
>
  <div class="pk-head">
    <span class="pk-kind">{card.kind}</span>
    {#if card.sim}<span class="pk-sim">SIM</span>{/if}
    {#if onDismiss}
      <button
        type="button"
        class="pk-dismiss"
        title="Remove from watchlist"
        aria-label="Remove from watchlist: {card.title}"
        onclick={dismiss}
        ondblclick={dismiss}
        onkeydown={onDismissKeydown}
      >×</button>
    {/if}
  </div>
  <div class="pk-title">{card.title}</div>
  <div class="pk-foot">
    <span class="pk-branch">{card.branch}</span>
    {#each shownTags as t (t)}<span class="pk-tag">{t}</span>{/each}
  </div>
  {#if card.skipReason}
    <div class="pk-skip">LEFT BEHIND — {card.skipReason}</div>
  {/if}
</div>

<style>
  .packet {
    background: var(--card, var(--ink, #12161c));
    border: 1px solid var(--hairline, #2a3138);
    border-left: 3px solid var(--pk);
    padding: 8px 12px;
    min-width: 0;
    cursor: pointer;
    transition: border-color 120ms ease;
  }
  /* The navigation affordance: the hairline takes the packet's own
     hue on hover; keyboard focus gets the same accent as an outline. */
  .packet:hover {
    border-color: var(--pk);
  }
  .packet:focus-visible {
    outline: 1px solid var(--pk);
    outline-offset: 2px;
  }
  .packet.sim {
    border-style: dashed;
    border-left-style: solid;
  }
  .pk-head {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  /* The dismiss control sits at the end of the head row, pushed right,
     so the × lands in the card's top corner clear of the kind chip.
     Quiet until hover/focus — an affordance the reader reaches for, not
     one that competes with the packet's own facts. Colors are the same
     status/text tokens the rest of the card reads. */
  .pk-dismiss {
    margin-left: auto;
    flex: none;
    background: none;
    border: none;
    cursor: pointer;
    padding: 0 2px;
    font-size: 15px;
    line-height: 1;
    color: var(--static, #7a838c);
    opacity: 0.55;
    transition:
      opacity 120ms ease,
      color 120ms ease;
  }
  .pk-dismiss:hover,
  .pk-dismiss:focus-visible {
    opacity: 1;
    color: var(--text, #1b1f23);
    outline: none;
  }
  .pk-kind {
    font-family: var(--font-mono, ui-monospace, monospace);
    font-size: 10px;
    letter-spacing: var(--ls-nav, 0.14em);
    text-transform: uppercase;
    color: var(--pk);
  }
  .pk-sim {
    font-family: var(--font-mono, ui-monospace, monospace);
    font-size: 10px;
    letter-spacing: 0.14em;
    color: var(--static, #7a838c);
    border: 1px dashed var(--static, #7a838c);
    padding: 0 5px;
  }
  .pk-title {
    font-size: 13.5px;
    margin: 3px 0 4px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .pk-foot {
    display: flex;
    align-items: center;
    gap: 6px;
    min-width: 0;
  }
  .pk-branch {
    font-family: var(--font-mono, ui-monospace, monospace);
    font-size: 11.5px;
    color: var(--static, #7a838c);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .pk-tag {
    font-family: var(--font-mono, ui-monospace, monospace);
    font-size: 10px;
    letter-spacing: 0.08em;
    color: var(--static, #7a838c);
    border: 1px solid var(--hairline, #2a3138);
    padding: 0 5px;
    flex: none;
  }
  .pk-skip {
    font-family: var(--font-mono, ui-monospace, monospace);
    font-size: 10.5px;
    color: var(--warn, #d9a441);
    margin-top: 4px;
    letter-spacing: 0.05em;
  }
  .packet.compact {
    padding: 5px 9px;
  }
  .packet.compact .pk-title {
    font-size: 12px;
    margin: 2px 0 2px;
    max-width: 260px;
  }
  .packet.compact .pk-foot {
    display: none;
  }
</style>
