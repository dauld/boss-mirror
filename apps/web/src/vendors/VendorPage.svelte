<script lang="ts">
  // Vendor detail — port of apps/web/src/vendors/VendorPage.tsx.
  //
  // Four-section KB surface per D2 of
  // examples/used-device-shop/design/procurement-team-needs.md:
  //   1. Profile (terms + account team + active contracts)
  //   2. People (contacts directory)
  //   3. Facts (interactions timeline)
  //   4. Work (POs + vendor invoices)

  import Breadcrumb from '@boss/web-kit/ui/Breadcrumb.svelte';
  import { formatDate } from '@boss/web-kit/ui/date';
  import EntityLink from '@boss/web-kit/ui/EntityLink.svelte';
  import Link from '@boss/web-kit/ui/Link.svelte';
  import Meta from '@boss/web-kit/ui/Meta.svelte';
  import ProvenanceBadge from '@boss/web-kit/ui/ProvenanceBadge.svelte';
  import Section from '@boss/web-kit/ui/Section.svelte';
  import { formatMoney } from '@boss/web-kit/ui/money';
  import { formatActor } from '../data/actor';
  import {
    loadVendorAccountTeam,
    loadVendorContacts,
    loadVendorContracts,
    loadVendorInteractions,
  } from './api';
  import {
    ACCOUNT_TEAM_ROLE_LABEL,
    CONTACT_ROLE_LABEL,
    CONTRACT_KIND_LABEL,
    INTERACTION_KIND_LABEL,
    type Vendor,
    type PurchaseOrder,
    type VendorInvoice,
    type VendorContact,
    type VendorInteraction,
    type VendorAccountTeamMember,
    type VendorContract,
  } from './types';
  import { href } from '../router';

  let { vendorLookup } = $props<{ vendorLookup: string }>();

  let vendors = $state<Vendor[]>([]);
  /// Non-null when the vendor list fetch FAILED — rendered instead of
  /// "Vendor not found", which is a claim only a successful lookup
  /// gets to make (packet 3fba9c35).
  let loadFailed = $state<string | null>(null);
  let pos = $state<PurchaseOrder[]>([]);
  let vendorInvoices = $state<VendorInvoice[]>([]);
  let empNames = $state<Map<string, string>>(new Map());
  let contacts = $state<VendorContact[]>([]);
  let interactions = $state<VendorInteraction[]>([]);
  let team = $state<VendorAccountTeamMember[]>([]);
  let contracts = $state<VendorContract[]>([]);
  let loading = $state(true);

  let lookup = $derived(decodeURIComponent(vendorLookup));
  let vendor = $derived<Vendor | undefined>(
    vendors.find((v) => v.id === lookup) ?? vendors.find((v) => v.name === lookup),
  );

  // Base data — vendors, POs, invoices, employees. Fetched once per
  // lookup change.
  $effect(() => {
    void lookup;
    let cancelled = false;
    loading = true;
    (async () => {
      try {
        const [vResp, pResp, iResp, peopleResp] = await Promise.all([
          fetch('/api/inventory/vendors'),
          fetch('/api/inventory/orders'),
          fetch('/api/inventory/vendor-invoices'),
          fetch('/api/people'),
        ]);
        const vBody = vResp.ok ? await vResp.json() : [];
        const pBody = pResp.ok ? await pResp.json() : [];
        const iBody = iResp.ok ? await iResp.json() : [];
        const peopleBody = peopleResp.ok ? await peopleResp.json() : [];
        if (!cancelled) {
          loadFailed = vResp.ok ? null : `HTTP ${vResp.status}`;
          vendors = Array.isArray(vBody) ? vBody : (vBody.data ?? []);
          pos = Array.isArray(pBody) ? pBody : (pBody.data ?? []);
          vendorInvoices = Array.isArray(iBody) ? iBody : (iBody.data ?? []);
          const names = new Map<string, string>();
          for (const e of peopleBody as Array<{ id: string; name: string }>) {
            names.set(e.id, e.name);
          }
          empNames = names;
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

  // CRM data — depends on the resolved vendor.id.
  $effect(() => {
    const vid = vendor?.id ?? null;
    let cancelled = false;
    (async () => {
      const [c, i, t, k] = await Promise.all([
        loadVendorContacts(vid),
        loadVendorInteractions(vid),
        loadVendorAccountTeam(vid),
        loadVendorContracts(vid),
      ]);
      if (!cancelled) {
        contacts = c;
        interactions = i;
        team = t;
        contracts = k;
      }
    })();
    return () => {
      cancelled = true;
    };
  });

  let vendorPos = $derived(
    vendor
      ? pos
          // POs store vendor as the stable vendor id (vnd-bigseed-001),
          // not the display name — filter by id, fall back to name for
          // any rows that pre-date the id convention.
          .filter((po) => po.vendor === vendor!.id || po.vendor === vendor!.name)
          .slice()
          .sort((a, b) => (b.placed_on ?? '').localeCompare(a.placed_on ?? ''))
      : [],
  );
  let openPos = $derived(
    vendorPos.filter((po) => po.status !== 'received' && po.status !== 'closed'),
  );
  let vendorBills = $derived(
    vendor
      ? vendorInvoices
          // Same id-vs-name mismatch as POs above — bills store the
          // vendor id; fall back to name for legacy rows.
          .filter((vi) => vi.vendor === vendor!.id || vi.vendor === vendor!.name)
          .slice()
          .sort((a, b) => (b.received_on ?? '').localeCompare(a.received_on ?? ''))
      : [],
  );
  let unpaidBills = $derived(vendorBills.filter((vi) => vi.status !== 'paid'));
  let outstandingCents = $derived(
    unpaidBills.reduce((sum, vi) => sum + vi.amount_cents, 0),
  );
  // Historical / lifetime metrics. Sim regens produce flows that
  // cycle through PO states quickly (place → receive → close in a
  // few sim-days), so the "open" counts often read zero while the
  // procurement relationship has been active for months. Surface
  // the lifetime totals + most-recent activity so the vendor page
  // tells the full story even when the in-flight queue is empty.
  let lifetimeSpendCents = $derived(
    vendorBills.reduce((sum, vi) => sum + vi.amount_cents, 0),
  );
  let lifetimePaidCents = $derived(
    vendorBills
      .filter((vi) => vi.status === 'paid')
      .reduce((sum, vi) => sum + vi.amount_cents, 0),
  );
  let lastPoDate = $derived(
    vendorPos.length === 0
      ? null
      : vendorPos
          .map((po) => po.placed_on)
          .filter((d): d is string => Boolean(d))
          .sort()
          .at(-1) ?? null,
  );
  let lastBillDate = $derived(
    vendorBills.length === 0
      ? null
      : vendorBills
          .map((vi) => vi.received_on)
          .filter((d): d is string => Boolean(d))
          .sort()
          .at(-1) ?? null,
  );
  let primaryContact = $derived(contacts.find((c) => c.is_primary));
  let activeContracts = $derived(contracts.filter((c) => c.status === 'active'));

</script>

{#if loading}
  <div class="catalog theme-exec">
    <p class="empty">Loading vendor…</p>
  </div>
{:else if loadFailed}
  <div class="catalog theme-exec">
    <p class="empty load-failed" role="alert">
      Couldn't load this vendor — {loadFailed}
    </p>
  </div>
{:else if !vendor}
  <div class="catalog theme-exec">
    <Breadcrumb to={href('/ux/warehouse')}>
      ← Warehouse
    </Breadcrumb>
    <div class="exec-header"><h1 class="exec-title">Vendor not found</h1></div>
    <p class="empty">No vendor record for <code>{lookup}</code>.</p>
  </div>
{:else}
  <div class="detail-page theme-exec">
    <Breadcrumb to={href('/ux/warehouse')}>
      ← Warehouse
    </Breadcrumb>

    <header class="detail-hero">
      <div>
        <div class="detail-eyebrow">
          <EntityLink kind="vendor" id={vendor.id} /> · {vendor.category?.replace(/-/g, ' ') ?? '—'}
        </div>
        <h1 class="detail-title">{vendor.name ?? vendor.id}</h1>
        <div class="detail-tagline">
          {primaryContact ? primaryContact.name : vendor.contact_name}
          {' · '}
          {vendor.city}, {vendor.state}
        </div>
        <div class="detail-meta">
          <Meta label="Open POs">{openPos.length}</Meta>
          <Meta label="POs lifetime">{vendorPos.length}</Meta>
          <Meta label="Unpaid bills">{unpaidBills.length}</Meta>
          <Meta label="Outstanding">{formatMoney({ amount_cents: outstandingCents, currency: 'USD' })}</Meta>
          <Meta label="Spend lifetime">{formatMoney({ amount_cents: lifetimeSpendCents, currency: 'USD' })}</Meta>
          <Meta label="Paid lifetime">{formatMoney({ amount_cents: lifetimePaidCents, currency: 'USD' })}</Meta>
          <Meta label="Last PO">{lastPoDate ?? '—'}</Meta>
          <Meta label="Last bill">{lastBillDate ?? '—'}</Meta>
          <Meta label="Lead time">{vendor.lead_time_days} days</Meta>
          <Meta label="Contacts">{contacts.length}</Meta>
          <Meta label="Active contracts">{activeContracts.length}</Meta>
        </div>
      </div>
    </header>

    <div class="subject-actions">
      <a
        class="action-btn"
        href={href(`/ux/jobs?new=1&subject_kind=vendor&subject_id=${encodeURIComponent(vendor.id)}`)}
      >
        + Create a Job for this vendor
      </a>
    </div>

    <!-- Section 1 — Profile -->
    <div class="tab-grid">
      <Section title="Terms">
          <dl class="kv">
            <dt>ID</dt><dd><EntityLink kind="vendor" id={vendor.id} /></dd>
            <dt>Category</dt><dd>{vendor.category?.replace(/-/g, ' ') ?? '—'}</dd>
            <dt>Payment terms</dt><dd>{vendor.payment_terms}</dd>
            <dt>Lead time</dt><dd>{vendor.lead_time_days} days</dd>
            <dt>Location</dt><dd>{vendor.city}, {vendor.state}</dd>
          </dl>
      </Section>

      <Section title="Behavior profile">
          {#if vendor.behavior}
            {@const b = vendor.behavior}
            <dl class="kv">
              <dt>Lead time</dt>
              <dd>
                {b.lead_time_days} ± {b.lead_spread_days} days
                <span style="margin-left:8px">
                  <ProvenanceBadge source={b.provenance.source} />
                </span>
              </dd>
              <dt>Fulfilment</dt><dd>{(b.fulfilment_rate * 100).toFixed(0)}%</dd>
              <dt>AP payment</dt><dd>{b.ap_payment_days} ± {b.ap_spread_days} days</dd>
            </dl>
            {#if b.provenance.template}
              <p class="empty" style="margin-top:8px">
                Bootstrapped from <strong>{b.provenance.template}</strong> template.
              </p>
            {/if}
          {:else}
            <p class="empty">No behavior profile (uncategorized vendor).</p>
          {/if}
      </Section>

      <Section title={`Account team (${team.length})`}>
          {#if team.length === 0}
            <p class="empty">No account-team assignments yet.</p>
          {:else}
            <dl class="kv">
              {#each team as m (m.id)}
                <dt>{ACCOUNT_TEAM_ROLE_LABEL[m.role] ?? m.role}</dt>
                <dd>
                  <EntityLink
                    kind="employee"
                    id={m.employee_id}
                    label={empNames.get(m.employee_id)}
                  />
                  {#if m.notes}
                    <span style="color:#78716c; margin-left:8px">· {m.notes}</span>
                  {/if}
                </dd>
              {/each}
            </dl>
          {/if}
      </Section>

      <Section title={`Active contracts (${activeContracts.length})`} wide>
          {#if activeContracts.length === 0}
            <p class="empty">No active contracts on file.</p>
          {:else}
            <table class="data-table">
              <thead>
                <tr>
                  <th>Kind</th>
                  <th>Title</th>
                  <th>Effective</th>
                  <th>Expires</th>
                  <th>Auto-renew</th>
                  <th>Signed by</th>
                </tr>
              </thead>
              <tbody>
                {#each activeContracts as c (c.id)}
                  <tr>
                    <td>{CONTRACT_KIND_LABEL[c.kind] ?? c.kind}</td>
                    <td>{c.title}</td>
                    <td>{c.effective_on}</td>
                    <td>{c.expires_on ?? '—'}</td>
                    <td>{c.auto_renew ? 'Yes' : 'No'}</td>
                    <td>
                      {#if c.signed_by_employee_id}
                        {@const signer = c.signed_by_employee_id}
                        <Link to={href(`/ux/hr/${signer}`)}>
                            {empNames.get(signer) ?? signer}
                        </Link>
                      {:else}
                        —
                      {/if}
                    </td>
                  </tr>
                {/each}
              </tbody>
            </table>
          {/if}
      </Section>
    </div>

    <!-- Section 2 — People -->
    <Section title={`Contacts (${contacts.length})`} wide>
        {#if contacts.length === 0}
          <p class="empty">
            No contacts captured yet. The legacy single-contact fields show
            <strong>{vendor.contact_name}</strong> ·
            <a href={`mailto:${vendor.contact_email}`}>{vendor.contact_email}</a>.
            Add contacts via POST /api/inventory/vendors/{vendor.id}/contacts.
          </p>
        {:else}
          <table class="data-table">
            <thead>
              <tr>
                <th>Name</th>
                <th>Role</th>
                <th>Email</th>
                <th>Phone</th>
                <th>Territory</th>
                <th>Specialties</th>
              </tr>
            </thead>
            <tbody>
              {#each contacts as c (c.id)}
                <tr>
                  <td>
                    {c.name}
                    {#if c.is_primary}
                      <span style="margin-left:6px; font-size:10px; padding:1px 6px; border-radius:3px; background:#16a34a22; color:#16a34a; font-weight:600">
                        PRIMARY
                      </span>
                    {/if}
                  </td>
                  <td>{CONTACT_ROLE_LABEL[c.role] ?? c.role}</td>
                  <td><a href={`mailto:${c.email}`}>{c.email}</a></td>
                  <td>{c.phone ?? '—'}</td>
                  <td>{c.territory ?? '—'}</td>
                  <td>{c.specialties.length > 0 ? c.specialties.join(', ') : '—'}</td>
                </tr>
              {/each}
            </tbody>
          </table>
        {/if}
    </Section>

    <!-- Section 3 — Facts (interactions) -->
    <Section title={interactions.length > 20 ? `Interactions (20 of ${interactions.length})` : `Interactions (${interactions.length})`} wide>
        {#if interactions.length === 0}
          <p class="empty">
            No interactions logged yet. Once procurement starts capturing calls, emails,
            and RFQs, they'll appear here as an append-only timeline.
          </p>
        {:else}
          <ul class="interaction-timeline" style="list-style:none; padding:0">
            {#each interactions.slice(0, 20) as i (i.id)}
              {@const contactName = contacts.find((c) => c.id === i.vendor_contact_id)?.name ?? null}
              <li style="border-left:2px solid #e7e5e4; padding:8px 12px; margin-bottom:8px; font-size:13px">
                <div style="display:flex; gap:8px; align-items:baseline">
                  <span style="font-size:11px; padding:1px 6px; border-radius:3px; background:#e7e5e4; color:#44403c; font-weight:500">
                    {INTERACTION_KIND_LABEL[i.kind] ?? i.kind}
                  </span>
                  <span style="color:#78716c">{formatDate(i.occurred_at)}</span>
                  <span style="color:#44403c">
                    by {formatActor(i.actor_id, empNames)}
                  </span>
                  {#if contactName}<span style="color:#44403c">with {contactName}</span>{/if}
                </div>
                <div style="margin-top:4px">{i.body}</div>
                {#if i.commitments.length > 0}
                  <ul style="margin-top:6px; font-size:12px; color:#44403c">
                    {#each i.commitments as c, idx (idx)}
                      <li>
                        ↳ {c.summary}
                        {#if c.due_by}<span style="color:#78716c"> — due {c.due_by}</span>{/if}
                        {#if c.linked_po_id}
                          {' · '}
                          <EntityLink kind="po" id={c.linked_po_id} />
                        {/if}
                      </li>
                    {/each}
                  </ul>
                {/if}
                {#if i.linked_po_id || i.linked_part_sku}
                  <div style="margin-top:4px; font-size:11px; color:#78716c">
                    {#if i.linked_po_id}
                      Linked PO: <EntityLink kind="po" id={i.linked_po_id} />
                    {/if}
                    {#if i.linked_po_id && i.linked_part_sku}{' · '}{/if}
                    {#if i.linked_part_sku}
                      Part: <span class="mono">{i.linked_part_sku}</span>
                    {/if}
                  </div>
                {/if}
              </li>
            {/each}
          </ul>
        {/if}
    </Section>

    <!-- Section 4 — Work -->
    <Section title={`Purchase orders (${vendorPos.length})`} wide>
        {#if vendorPos.length === 0}
          <p class="empty">No purchase orders for this vendor yet.</p>
        {:else}
          <table class="data-table">
            <thead>
              <tr><th>PO</th><th>Status</th><th>Placed</th><th>Expected</th><th>Received</th><th class="num">Lines</th></tr>
            </thead>
            <tbody>
              {#each vendorPos as po (po.id)}
                <tr>
                  <td class="mono"><EntityLink kind="po" id={po.id} /></td>
                  <td>{po.status.replace(/-/g, ' ')}</td>
                  <td>{po.placed_on}</td>
                  <td>{po.expected_on}</td>
                  <td>{po.received_on ?? '—'}</td>
                  <td class="num">{po.lines.length}</td>
                </tr>
              {/each}
            </tbody>
          </table>
        {/if}
    </Section>

    <Section title={`Vendor invoices (${vendorBills.length})`} wide>
        {#if vendorBills.length === 0}
          <p class="empty">No invoices received from this vendor yet.</p>
        {:else}
          <table class="data-table">
            <thead>
              <tr><th>Invoice #</th><th>PO</th><th>Received</th><th>Status</th><th class="num">Amount</th></tr>
            </thead>
            <tbody>
              {#each vendorBills as vi (vi.id)}
                <tr>
                  <td class="mono"><EntityLink kind="vendor-invoice" id={vi.id} label={vi.vendor_invoice_no} /></td>
                  <td class="mono"><EntityLink kind="po" id={vi.po_id} /></td>
                  <td>{vi.received_on}</td>
                  <td>{vi.status}</td>
                  <td class="num">{formatMoney({ amount_cents: vi.amount_cents, currency: vi.currency })}</td>
                </tr>
              {/each}
            </tbody>
          </table>
        {/if}
    </Section>
  </div>
{/if}
