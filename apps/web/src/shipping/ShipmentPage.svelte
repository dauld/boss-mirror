<script lang="ts">
  // Shipment detail — port of apps/web/src/shipping/ShipmentPage.tsx.

  import Breadcrumb from '@boss/web-kit/ui/Breadcrumb.svelte';
  import EntityLink from '@boss/web-kit/ui/EntityLink.svelte';
  import Meta from '@boss/web-kit/ui/Meta.svelte';
  import Section from '@boss/web-kit/ui/Section.svelte';
  import {
    CARRIER_LABEL,
    DIRECTION_LABEL,
    STATUS_LABEL,
    type Shipment,
  } from './types';
  import { href } from '../router';
  import { fetchPaged } from '../data/paginated';
  import { ACCOUNTS_LIST_URL } from '../accounts/api';

  type Account = { id: string; name: string };

  let { shipmentId } = $props<{ shipmentId: string }>();

  let shipment = $state<Shipment | null>(null);
  let accounts = $state<Account[]>([]);
  /// Non-null when the record fetch FAILED (5xx / network) — rendered
  /// instead of "Shipment not found", which is a claim only a
  /// successful lookup gets to make (packet 3fba9c35).
  let loadFailed = $state<string | null>(null);
  let loading = $state(true);

  let id = $derived(decodeURIComponent(shipmentId));

  $effect(() => {
    const targetId = id;
    let cancelled = false;
    loading = true;
    (async () => {
      try {
        const [sResp, pPaged] = await Promise.all([
          fetch(`/api/shipping/shipments/${encodeURIComponent(targetId)}`),
          fetchPaged<Account>(ACCOUNTS_LIST_URL),
        ]);
        const sBody = sResp.ok ? ((await sResp.json()) as Shipment) : null;
        if (!cancelled) {
          shipment = sBody;
          loadFailed =
            sResp.ok || sResp.status === 404 ? null : `HTTP ${sResp.status}`;
          accounts = pPaged.kind === 'ready' ? [...pPaged.page.data] : [];
          loading = false;
        }
      } catch (e) {
        if (!cancelled) {
          loadFailed = e instanceof Error ? e.message : String(e);
          loading = false;
        }
      }
    })();
    return () => {
      cancelled = true;
    };
  });

  let account = $derived(
    shipment?.account_id
      ? accounts.find((p) => p.id === shipment!.account_id)
      : null,
  );
</script>

{#if loading}
  <div class="catalog theme-exec">
    <p class="empty">Loading shipment…</p>
  </div>
{:else if loadFailed}
  <div class="catalog theme-exec">
    <Breadcrumb to={href('/ux/shipping')}>
      ← Shipping
    </Breadcrumb>
    <p class="empty load-failed" role="alert">
      Couldn't load this shipment — {loadFailed}
    </p>
  </div>
{:else if !shipment}
  <div class="catalog theme-exec">
    <Breadcrumb to={href('/ux/shipping')}>
      ← Shipping
    </Breadcrumb>
    <div class="exec-header"><h1 class="exec-title">Shipment not found</h1></div>
    <p class="empty">No shipment record for <code>{id}</code>.</p>
  </div>
{:else}
  {@const s = shipment}
  <div class="detail-page theme-exec">
    <Breadcrumb to={href('/ux/shipping')}>
      ← Shipping
    </Breadcrumb>

    <header class="detail-hero">
      <div>
        <div class="detail-eyebrow">
          <EntityLink kind="shipment" id={s.id} /> · {DIRECTION_LABEL[s.direction]} ·
          {STATUS_LABEL[s.status]}
        </div>
        <h1 class="detail-title">{s.origin} → {s.destination}</h1>
        <div class="detail-tagline">
          {s.carrier ? CARRIER_LABEL[s.carrier] : '—'}
          {#if s.tracking_number} · <span class="mono">{s.tracking_number}</span>{/if}
        </div>
        <div class="detail-meta">
          <Meta label="Created">{s.created_on}</Meta>
          <Meta label="Shipped">{s.shipped_on ?? '—'}</Meta>
          <Meta label="ETA">{s.estimated_delivery ?? '—'}</Meta>
          <Meta label="Delivered">{s.delivered_on ?? '—'}</Meta>
        </div>
      </div>
    </header>

    <div class="tab-grid">
      <Section title="Summary">
          <dl class="kv">
            <dt>Direction</dt><dd>{DIRECTION_LABEL[s.direction]}</dd>
            <dt>Status</dt><dd>{STATUS_LABEL[s.status]}</dd>
            <dt>Carrier</dt><dd>{s.carrier ? CARRIER_LABEL[s.carrier] : '—'}</dd>
            <dt>Tracking</dt><dd>{s.tracking_number ?? '—'}</dd>
            <dt>Origin</dt><dd>{s.origin}</dd>
            <dt>Destination</dt><dd>{s.destination}</dd>
            <dt>Account</dt>
            <dd>
              {#if account}
                <EntityLink kind="account" id={account.id} label={account.name} />
              {:else if s.account_id}
                <EntityLink kind="account" id={s.account_id} />
              {:else}
                —
              {/if}
            </dd>
            <dt>Purchase order</dt>
            <dd>
              {#if s.po_id}
                <EntityLink kind="po" id={s.po_id} />
              {:else}
                —
              {/if}
            </dd>
            <dt>Order</dt><dd>{s.order_id ?? '—'}</dd>
          </dl>
      </Section>

      <Section title={`Systems (${s.asset_ids.length})`} wide>
          {#if s.asset_ids.length === 0}
            <p class="empty">No systems on this shipment.</p>
          {:else}
            <table class="data-table">
              <thead>
                <tr><th>System</th></tr>
              </thead>
              <tbody>
                {#each s.asset_ids as sid (sid)}
                  <tr>
                    <td class="mono"><EntityLink kind="asset" id={sid} /></td>
                  </tr>
                {/each}
              </tbody>
            </table>
          {/if}
      </Section>
    </div>
  </div>
{/if}
