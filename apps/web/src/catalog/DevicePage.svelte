<script lang="ts">
  // Catalog device detail — port of apps/web/src/catalog/DevicePage.tsx.

  import Breadcrumb from '@boss/web-kit/ui/Breadcrumb.svelte';
  import Meta from '@boss/web-kit/ui/Meta.svelte';
  import NotFound from '@boss/web-kit/ui/NotFound.svelte';
  import SortHeader from '@boss/web-kit/ui/SortHeader.svelte';
  import StatusChip from '@boss/web-kit/ui/StatusChip.svelte';
  import { formatDate } from '@boss/web-kit/ui/date';
  import { entityHref } from '@boss/web-kit/ui/entity-href';
  import { formatMoney } from '@boss/web-kit/ui/money';
  import { rowLink } from '@boss/web-kit/ui/RowLink';
  import { createSortState } from '@boss/web-kit/ui/sort-state.svelte';
  import { CATEGORY_LABEL } from './types';
  import { DeviceModelSchema, type DeviceModel } from './schemas';
  import { fetchValidated } from '../data/parseResponse';
  import { href, navigate } from '../router';

  type Props = { sku: string };
  let { sku }: Props = $props();

  let device = $state<DeviceModel | null>(null);
  let loading = $state(true);
  let parseError = $state<string | null>(null);

  $effect(() => {
    const s = sku;
    let cancelled = false;
    loading = true;
    parseError = null;
    (async () => {
      const result = await fetchValidated(
        `/api/catalog/models/${encodeURIComponent(s)}`,
        DeviceModelSchema,
      );
      if (cancelled) return;
      if (result.kind === 'ok') {
        device = result.data;
      } else if (result.kind === 'invalid') {
        // Server returned 200 but the payload doesn't match the
        // schema — likely a server-side bug. Surface explicitly
        // instead of silently rendering null.
        parseError = result.reason;
        device = null;
      } else {
        // HTTP error / 404 — render the not-found state.
        device = null;
      }
      loading = false;
    })();
    return () => {
      cancelled = true;
    };
  });

  // Typed-vs-extras boundary for Equipment KB:
  // `extras` is a tenant-defined `Record<string, unknown>`. The
  // page must NOT cast it to a tenant-specific shape — that's how
  // prior-tenant laser-equipment keys (wavelengths_nm, max_energy_mj,
  // fluence_j_per_cm2, beam_safety_class, …) leaked into the
  // playground 2026-05-25. Render extras generically: pretty-print
  // each key as a humanized label, render values per shape (array
  // → comma-joined, range object → "min – max", primitive → str).

  type Physical = {
    width_cm: number;
    depth_cm: number;
    height_cm: number;
    weight_kg: number;
    power_requirements: string;
  };
  type Regulatory = {
    clearance_id: string | null;
    clearance_date: string | null;
    regulator_device_class: number;
  };

  // Format a key as a human label. `port_count_gigabit` →
  // "Port count gigabit". Conservative: only fixes underscores
  // + capitalization; doesn't try to be clever about units.
  function humanizeKey(k: string): string {
    const spaced = k.replace(/_/g, ' ').replace(/-/g, ' ');
    return spaced.charAt(0).toUpperCase() + spaced.slice(1);
  }

  // Format a value generically. Arrays → comma-joined; objects with
  // {min,max} → "min – max"; everything else → String(value).
  function formatExtraValue(v: unknown): string {
    if (v == null) return '—';
    if (Array.isArray(v)) return v.map((x) => formatExtraValue(x)).join(', ');
    if (typeof v === 'object') {
      const o = v as Record<string, unknown>;
      if ('min' in o && 'max' in o) return `${o['min']} – ${o['max']}`;
      // Fallback for unknown object shape — JSON-ish so the data is
      // readable without committing to a structure.
      return JSON.stringify(v);
    }
    return String(v);
  }

  // Failure-modes table sort (web-kit sortState pilot). Frequency
  // stays the landing order, descending — the previous hardcoded
  // sort — but the columns are clickable now.
  const fmSort = createSortState<'code' | 'name' | 'frequency'>(
    { key: 'frequency', dir: 'desc' },
    (k) => (k === 'frequency' ? 'desc' : 'asc'),
  );
</script>

{#if loading}
  <div class="catalog theme-exec">
    <p class="empty">Loading device…</p>
  </div>
{:else if parseError}
  <div class="catalog theme-exec">
    <NotFound
      eyebrow="Knowledge Base"
      title="Server returned an unexpected payload shape"
      backHref={href('/ux/catalog')}
      backLabel="All devices"
    >
      The catalog API responded with a body that didn't match the
      expected schema for SKU <code>{sku}</code>. This is a
      server-side bug — the payload is likely missing a required
      field or has an unexpected type. Details:
    </NotFound>
    <pre class="empty">{parseError}</pre>
  </div>
{:else if !device}
  <div class="catalog theme-exec">
    <NotFound
      eyebrow="Knowledge Base"
      title="Device not found"
      backHref={href('/ux/catalog')}
      backLabel="All devices"
    >
      No catalog entry with SKU <code>{sku}</code>.
    </NotFound>
  </div>
{:else}
  {@const d = device}
  <!-- Coalesce-then-cast for the presentation-layer optionals.
       Brewery `BREW-BARREL-*` models have no `extras` / `physical`
       / `regulatory`; `extras` stays as `Record<string, unknown>`
       (the tenant-defined shape) — DO NOT cast it to
       a typed alias, that's the leak-vector this page got bitten
       by 2026-05-25 (laser-equipment keys hardcoded for a prior
       tenant). Render generically via humanizeKey + formatExtraValue. -->
  {@const ex = (d.extras ?? {}) as Record<string, unknown>}
  {@const ph = (d.physical ?? {}) as unknown as Physical}
  {@const reg = (d.regulatory ?? {}) as unknown as Regulatory}
  {@const s = d.service}

  <div class="detail-page theme-exec">
    <Breadcrumb to={href('/ux/catalog')}>
      ← All devices
    </Breadcrumb>

    <header class="detail-hero">
      <div>
        <div class="detail-eyebrow">{CATEGORY_LABEL[d.category]} · {d.model_year}</div>
        <h1 class="detail-title">{d.name}</h1>
        <div class="detail-tagline">{d.commerce.tagline}</div>
        <div class="detail-meta">
          <Meta label="SKU" mono={true}>{d.sku}</Meta>
          <Meta label="Manufacturer">{d.manufacturer}</Meta>
          <Meta label="List price">
              {formatMoney({ amount_cents: d.commerce.list_price_new_cents, currency: d.commerce.currency })}
          </Meta>
          {#if d.commerce.typical_refurb_price_cents !== null}
            {@const refurbCents = d.commerce.typical_refurb_price_cents}
            <Meta label="Refurb typical">
                {formatMoney({ amount_cents: refurbCents, currency: d.commerce.currency })}
            </Meta>
          {/if}
        </div>
      </div>
      <div class="detail-hero-tile" data-category={d.category}></div>
    </header>

    <div class="tab-grid">
      <section class="tab-section tab-section-wide">
        <h3>About</h3>
        <p class="prose">{d.commerce.description}</p>
      </section>

      <section class="tab-section">
        <h3>Use cases</h3>
        <div class="chips">
          {#each d.commerce.use_cases as u (u)}
            <span class="chip">{u}</span>
          {/each}
        </div>
      </section>

      <section class="tab-section">
        <h3>At a glance</h3>
        <dl class="kv">
          <dt>Clearance ID</dt>
          <dd>{reg.clearance_id ?? '—'}</dd>
          <dt>Firmware</dt>
          <dd>{d.current_firmware ?? '—'}</dd>
          <dt>List price</dt>
          <dd>{formatMoney({ amount_cents: d.commerce.list_price_new_cents, currency: d.commerce.currency })}</dd>
          <dt>Lead time</dt>
          <dd>{d.commerce.lead_time_days} days</dd>
        </dl>
      </section>

      <section class="tab-section">
        <h3>Specs</h3>
        {#if Object.keys(ex).length === 0}
          <p class="empty">No model-specific specs published.</p>
        {:else}
          <dl class="kv">
            {#each Object.entries(ex) as [key, value] (key)}
              <dt>{humanizeKey(key)}</dt>
              <dd>{formatExtraValue(value)}</dd>
            {/each}
          </dl>
        {/if}
      </section>

      <section class="tab-section">
        <h3>Physical & power</h3>
        <dl class="kv">
          <dt>Dimensions</dt>
          <dd>
            {ph.width_cm} × {ph.depth_cm} × {ph.height_cm} cm (W×D×H)
          </dd>
          <dt>Weight</dt><dd>{ph.weight_kg} kg</dd>
          <dt>Power</dt><dd>{ph.power_requirements}</dd>
        </dl>
      </section>

      <section class="tab-section">
        <h3>Regulatory</h3>
        <dl class="kv">
          <dt>Clearance ID</dt><dd>{reg.clearance_id ?? '—'}</dd>
          <dt>Clearance date</dt>
          <dd>{reg.clearance_date ? formatDate(reg.clearance_date) : '—'}</dd>
          <dt>Device class</dt><dd>Class {reg.regulator_device_class}</dd>
        </dl>
      </section>

      <section class="tab-section">
        <h3>Service profile</h3>
        <dl class="kv">
          <dt>preventive maintenance interval</dt><dd>every {s.preventive_maintenance_interval_months} months</dd>
          <dt>preventive maintenance duration</dt><dd>{s.preventive_maintenance_hours} hours typical</dd>
          <dt>Calibration interval</dt><dd>every {s.calibration_interval_months} months</dd>
          <dt>Skill level required</dt><dd>{s.required_skill_level} / 5</dd>
          <dt>Serviceable</dt>
          <dd>{s.depot_required ? 'depot only' : 'field or depot'}</dd>
        </dl>
      </section>

      <section class="tab-section">
        <h3>preventive maintenance checklist</h3>
        <ol class="checklist">
          {#each s.pm_checklist as item, i (i)}
            <li>{item}</li>
          {/each}
        </ol>
      </section>

      <section class="tab-section tab-section-wide">
        <h3>Common failure modes</h3>
        {#if s.common_failure_modes.length === 0}
          <p class="empty">No failure modes documented for this model.</p>
        {:else}
          {@const sorted = fmSort.sorted(s.common_failure_modes, {
            code: (fm) => fm.code,
            name: (fm) => fm.name,
            frequency: (fm) => fm.frequency,
          })}
          <table class="data-table">
            <thead>
              <tr>
                <SortHeader sort={fmSort} key="code">Code</SortHeader>
                <SortHeader sort={fmSort} key="name">Symptom</SortHeader>
                <SortHeader sort={fmSort} key="frequency" num={true}>Frequency</SortHeader>
                <th>Typical fix</th>
              </tr>
            </thead>
            <tbody>
              {#each sorted as fm (fm.code)}
                <tr>
                  <td class="mono">{fm.code}</td>
                  <td>{fm.name}</td>
                  <td class="num">{(fm.frequency * 100).toFixed(1)}%</td>
                  <td class="prose-cell">{fm.typical_fix}</td>
                </tr>
              {/each}
            </tbody>
          </table>
        {/if}
      </section>

      <section class="tab-section tab-section-wide">
        <h3>Spare parts ({d.spare_parts.length})</h3>
        {#if d.spare_parts.length === 0}
          <p class="empty">No spare parts listed.</p>
        {:else}
          <table class="data-table">
            <thead>
              <tr>
                <th>SKU</th><th>Name</th><th>Description</th><th>Price</th><th>Lead time</th><th>Usage</th>
              </tr>
            </thead>
            <tbody>
              {#each d.spare_parts as p (p.part_sku)}
                <!-- rowLink pilot: the whole row opens the part's
                     inventory page (same target PartsList rows use). -->
                <tr
                  use:rowLink={{
                    onActivate: () => navigate(entityHref('part', p.part_sku)),
                    label: `${p.name} (${p.part_sku})`,
                  }}
                >
                  <td class="mono">{p.part_sku}</td>
                  <td>{p.name}</td>
                  <td class="prose-cell">{p.description}</td>
                  <td class="num">{formatMoney({ amount_cents: p.unit_price_cents, currency: p.currency })}</td>
                  <td class="num">{p.lead_time_days}d</td>
                  <td>
                    {#if p.high_usage}
                      <StatusChip value="high" tone="warn" />
                    {:else}
                      <StatusChip value="normal" tone="muted" />
                    {/if}
                  </td>
                </tr>
              {/each}
            </tbody>
          </table>
        {/if}
      </section>

      {#if d.consumables.length > 0}
        <section class="tab-section tab-section-wide">
          <h3>Consumables ({d.consumables.length})</h3>
          <table class="data-table">
            <thead>
              <tr>
                <th>SKU</th><th>Name</th><th>Description</th><th>Price</th><th>Per unit</th>
              </tr>
            </thead>
            <tbody>
              {#each d.consumables as c (c.part_sku)}
                <tr>
                  <td class="mono">{c.part_sku}</td>
                  <td>{c.name}</td>
                  <td class="prose-cell">{c.description}</td>
                  <td class="num">{formatMoney({ amount_cents: c.unit_price_cents, currency: c.currency })}</td>
                  <td class="num">{c.treatments_per_unit ?? '—'}</td>
                </tr>
              {/each}
            </tbody>
          </table>
        </section>
      {/if}

      <section class="tab-section tab-section-wide">
        <h3>Documents ({d.documents.length})</h3>
        {#if d.documents.length === 0}
          <p class="empty">No documents attached to this model.</p>
        {:else}
          <table class="data-table">
            <thead>
              <tr>
                <th>Kind</th><th>Title</th><th>Audience</th><th>Version</th><th>Published</th>
              </tr>
            </thead>
            <tbody>
              {#each d.documents as doc, i (i)}
                <tr>
                  <td>{doc.kind.replace(/-/g, ' ')}</td>
                  <td><a href={doc.url}>{doc.title}</a></td>
                  <td>{doc.audience}</td>
                  <td>{doc.version ?? '—'}</td>
                  <td>{doc.published ? formatDate(doc.published) : '—'}</td>
                </tr>
              {/each}
            </tbody>
          </table>
        {/if}
      </section>
    </div>
  </div>
{/if}
