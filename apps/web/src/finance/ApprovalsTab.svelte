<script lang="ts">
  // PO approvals — port of ApprovalsTab from FinancePage.tsx. Draft POs
  // can be approved (moves them to 'submitted').

  import EntityLink from '@boss/web-kit/ui/EntityLink.svelte';
  import Section from '@boss/web-kit/ui/Section.svelte';
  import { formatMoney } from '@boss/web-kit/ui/money';

  type PurchaseOrderLine = {
    part_sku: string;
    qty: number;
    unit_cost_cents: number;
    currency: string;
  };
  type PurchaseOrder = {
    id: string;
    vendor: string;
    status: string;
    placed_on: string;
    expected_on: string;
    lines: ReadonlyArray<PurchaseOrderLine>;
  };

  let orders = $state<PurchaseOrder[]>([]);
  /// Non-null when the load failed — rendered instead of the empty
  /// state, so an outage never reads as "nothing awaiting approval"
  /// (packet 3fba9c35, the false-empty sweep).
  let loadFailed = $state<string | null>(null);
  let loading = $state(true);
  let actionStatus = $state<Record<string, string>>({});

  $effect(() => {
    let cancelled = false;
    loading = true;
    (async () => {
      try {
        const r = await fetch('/api/inventory/orders');
        if (r.ok) {
          const all = (await r.json()) as PurchaseOrder[];
          if (!cancelled) {
            orders = all;
            loadFailed = null;
          }
        } else {
          if (!cancelled) loadFailed = `HTTP ${r.status}`;
        }
      } catch (e) {
        if (!cancelled) loadFailed = e instanceof Error ? e.message : String(e);
      }
      if (!cancelled) loading = false;
    })();
    return () => {
      cancelled = true;
    };
  });

  async function approvePo(poId: string): Promise<void> {
    actionStatus = { ...actionStatus, [poId]: 'approving...' };
    try {
      const r = await fetch(
        `/api/inventory/orders/${encodeURIComponent(poId)}/status`,
        {
          method: 'PUT',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ status: 'submitted' }),
        },
      );
      if (r.ok) {
        actionStatus = { ...actionStatus, [poId]: 'approved' };
        orders = orders.map((po) =>
          po.id === poId ? { ...po, status: 'submitted' } : po,
        );
      } else {
        actionStatus = { ...actionStatus, [poId]: `error: ${r.status}` };
      }
    } catch (e) {
      actionStatus = {
        ...actionStatus,
        [poId]: `error: ${e instanceof Error ? e.message : 'unknown'}`,
      };
    }
  }

  let draftOrders = $derived(orders.filter((po) => po.status === 'draft'));
  let recentApproved = $derived(
    orders.filter((po) => po.status === 'submitted').slice(0, 5),
  );
</script>

{#if loading}
  <p style="padding:16px; color:var(--static)">Loading purchase orders...</p>
{:else if loadFailed}
  <p class="empty load-failed" role="alert" style="padding:16px">
    Couldn't load purchase orders — {loadFailed}
  </p>
{:else}
  <div style="padding:0 0 32px">
    <Section title={`Pending approval (${draftOrders.length})`}>
        {#if draftOrders.length === 0}
          <p class="empty">No purchase orders awaiting approval.</p>
        {:else}
          <table class="data-table data-table-striped">
            <thead>
              <tr>
                <th>PO ID</th>
                <th>Vendor</th>
                <th>Parts</th>
                <th>Total</th>
                <th>Placed</th>
                <th></th>
              </tr>
            </thead>
            <tbody>
              {#each draftOrders as po (po.id)}
                {@const totalCents = po.lines.reduce((s, l) => s + l.qty * l.unit_cost_cents, 0)}
                {@const poCurrency = po.lines[0]?.currency ?? 'USD'}
                {@const status = actionStatus[po.id]}
                <tr>
                  <td class="mono"><EntityLink kind="po" id={po.id} /></td>
                  <td><EntityLink kind="vendor" id={po.vendor} /></td>
                  <td class="prose-cell">
                    {po.lines.map((l) => `${l.part_sku} x${l.qty}`).join(', ')}
                  </td>
                  <td class="num">
                    {formatMoney({ amount_cents: totalCents, currency: poCurrency })}
                  </td>
                  <td>{po.placed_on}</td>
                  <td>
                    {#if status === 'approved'}
                      <span style="color:var(--ok); font-size:12px">Approved</span>
                    {:else}
                      <button
                        class="hr-done-btn"
                        onclick={() => approvePo(po.id)}
                        disabled={status === 'approving...'}
                      >
                        {status === 'approving...' ? 'Approving...' : 'Approve'}
                      </button>
                    {/if}
                  </td>
                </tr>
              {/each}
            </tbody>
          </table>
        {/if}
    </Section>

    {#if recentApproved.length > 0}
      <Section title="Recently approved">
          <table class="data-table data-table-striped">
            <thead>
              <tr>
                <th>PO ID</th>
                <th>Vendor</th>
                <th>Parts</th>
                <th>Status</th>
              </tr>
            </thead>
            <tbody>
              {#each recentApproved as po (po.id)}
                <tr>
                  <td class="mono"><EntityLink kind="po" id={po.id} /></td>
                  <td><EntityLink kind="vendor" id={po.vendor} /></td>
                  <td class="prose-cell">
                    {po.lines.map((l) => `${l.part_sku} x${l.qty}`).join(', ')}
                  </td>
                  <td><span class="chip chip-health-check">submitted</span></td>
                </tr>
              {/each}
            </tbody>
          </table>
      </Section>
    {/if}
  </div>
{/if}
