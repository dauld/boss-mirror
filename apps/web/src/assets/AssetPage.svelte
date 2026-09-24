<script lang="ts">
  // Asset detail page.
  //
  // Fetches the device current state + event log + the open Jobs
  // about it, of any kind (the query carries subject_id, not a
  // workflow kind). The DeviceInsights panel over
  // /api/assets/{id}/insights was never built here; that endpoint's
  // service-history section, which read the device shop's
  // `field-service` Jobs, was retired unread (backlog a8991c86,
  // car 3).

  import { href, navigate } from '../router';
  import { entityHref } from '@boss/web-kit/ui/entity-href';
  import { getLabel } from '@boss/web-kit/session/manifest.svelte';
  import { shortId } from '../data/ids';
  import { formatActor } from '../data/actor';
  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import Section from '@boss/web-kit/ui/Section.svelte';
  import type { Asset, AssetEvent } from './types';
  import type { Job } from '../jobs/types';
  import { AssetDetailSchema, AssetPartListSchema } from './schemas';
  import { JobSchema } from '../accounts/schemas';
  import { fetchValidated, fetchPagedValidated } from '../data/parseResponse';

  let { assetId } = $props<{ assetId: string }>();

  // Wave 9: Part shape mirrors boss-core::primitives::Part. The
  // aggregator on `/api/assets/{id}/parts` emits software
  // configs + accessories today; future kinds (firmware-update
  // history, license transfers, etc.) show up automatically.
  type Part =
    | { part_kind: 'subject'; subject_kind: string; id: string }
    | { part_kind: 'attribute'; key: string; value: Record<string, unknown> };
  type SoftwareConfigValue = {
    firmware_version?: string;
    modules?: string[];
    license_tier?: string;
    last_updated_on?: string | null;
  };
  type AccessoryValue = {
    accessory_kind?: string;
    serial?: string | null;
    installed_on?: string | null;
    notes?: string | null;
  };

  let device = $state<Asset | null>(null);
  let events = $state<AssetEvent[]>([]);
  let openJobs = $state<Job[]>([]);
  let parts = $state<Part[]>([]);
  let loading = $state(true);
  let error = $state<string | null>(null);

  $effect(() => {
    const sid = assetId;
    let cancelled = false;
    loading = true;
    (async () => {
      // /api/assets/{serial} returns { current_state, events } as a
      // single composite payload — matches the React app's
      // useAsset. There's no /systems/ segment on the detail
      // path (see crates/modules/boss-assets/src/http.rs routes).
      const devUrl = `/api/assets/${encodeURIComponent(sid)}`;
      const jobUrl = `/api/jobs?subject_id=${encodeURIComponent(sid)}&status=open&limit=20`;
      const partsUrl = `/api/assets/${encodeURIComponent(sid)}/parts`;
      const [devRes, jobRes, partsRes] = await Promise.all([
        fetchValidated(devUrl, AssetDetailSchema),
        fetchPagedValidated(jobUrl, JobSchema),
        fetchValidated(partsUrl, AssetPartListSchema),
      ]);
      if (cancelled) return;
      for (const r of [devRes, jobRes, partsRes]) {
        if (r.kind === 'invalid') {
          error = r.reason;
          loading = false;
          return;
        }
      }
      if (devRes.kind === 'ok') {
        device = devRes.data.current_state as Asset | null;
        events = devRes.data.events as AssetEvent[];
      } else {
        // HTTP error — device 404 surfaces here as 'error', which the
        // template renders as "Device not found / error".
        error = devRes.reason;
        loading = false;
        return;
      }
      openJobs = jobRes.kind === 'ok' ? (jobRes.data.data as unknown as Job[]) : [];
      parts = partsRes.kind === 'ok' ? (partsRes.data as Part[]) : [];
      loading = false;
      error = null;
    })();
    return () => {
      cancelled = true;
    };
  });

  // Split the Parts list into the two current consumers. A future
  // KnowledgeBaseView (Wave 10) generalises this dispatch.
  let softwareConfig = $derived.by<SoftwareConfigValue | null>(() => {
    for (const p of parts) {
      if (p.part_kind === 'attribute' && p.key === 'software_config') {
        return p.value as SoftwareConfigValue;
      }
    }
    return null;
  });
  let accessories = $derived.by<AccessoryValue[]>(() =>
    parts
      .filter((p): p is Extract<Part, { part_kind: 'attribute' }> =>
        p.part_kind === 'attribute' && p.key === 'accessory',
      )
      .map((p) => p.value as AccessoryValue),
  );
</script>

{#if loading}
  <div class="catalog theme-exec"><p class="empty">Loading…</p></div>
{:else if error || !device}
  <div class="catalog theme-exec">
    <p class="empty load-failed" role="alert">Couldn't load device: {error ?? 'not found'}</p>
  </div>
{:else}
  <div class="detail-page theme-exec">
    <PageHeader
      eyebrow={`${getLabel('nav.assets_label', 'Assets')} · ${getLabel('assets.entity_singular', 'Asset')}`}
      title={device.asset_id}
      subtitle={`${device.sku ?? 'unidentified'} · phase ${device.phase}${device.holder_id ? ` · held by ${device.holder_kind ?? '?'}/${device.holder_id}` : ''}`}
    />

    <div class="subject-actions">
      <a
        class="action-btn"
        href={href(`/ux/jobs?new=1&subject_kind=system&subject_id=${encodeURIComponent(device.asset_id)}`)}
      >
        + Create a Job for this device
      </a>
    </div>

    <div class="tab-grid">
      <Section title="Current state">
        <div class="jd-info-row">
          <span class="jd-info-label">BOSS ID</span>
          <span class="jd-info-value jd-mono">{device.asset_id}</span>
        </div>
        <div class="jd-info-row">
          <span class="jd-info-label">{getLabel('assets.serial_label', 'OEM serial')}</span>
          <span class="jd-info-value jd-mono">{device.oem_serial ?? '—'}</span>
        </div>
        <div class="jd-info-row">
          <span class="jd-info-label">Phase</span>
          <span class="jd-info-value">{device.phase}</span>
        </div>
        <div class="jd-info-row">
          <span class="jd-info-label">Warranty</span>
          <span class="jd-info-value">{device.warranty_through ?? 'out'}</span>
        </div>
        <div class="jd-info-row">
          <span class="jd-info-label">Open SRs</span>
          <span class="jd-info-value">{device.open_ticket_count}</span>
        </div>
        <div class="jd-info-row">
          <span class="jd-info-label">First seen</span>
          <span class="jd-info-value">{device.first_seen}</span>
        </div>
      </Section>

      <Section title="Software & accessories">
        {#if softwareConfig}
          <div class="jd-info-row">
            <span class="jd-info-label">Firmware</span>
            <span class="jd-info-value jd-mono" data-testid="device-firmware">
              {softwareConfig.firmware_version ?? '—'}
            </span>
          </div>
          <div class="jd-info-row">
            <span class="jd-info-label">License</span>
            <span class="jd-info-value">
              {softwareConfig.license_tier ?? '—'}
            </span>
          </div>
          <div class="jd-info-row">
            <span class="jd-info-label">Modules</span>
            <span class="jd-info-value">
              {(softwareConfig.modules ?? []).join(', ') || '—'}
            </span>
          </div>
          {#if softwareConfig.last_updated_on}
            <div class="jd-info-row">
              <span class="jd-info-label">Last updated</span>
              <span class="jd-info-value">
                {softwareConfig.last_updated_on}
              </span>
            </div>
          {/if}
        {:else}
          <p class="empty">No software config recorded.</p>
        {/if}

        <div style="margin-top: 12px; font-weight: 500;">
          Installed accessories ({accessories.length})
        </div>
        {#if accessories.length === 0}
          <p class="empty">No accessories installed.</p>
        {:else}
          <ul data-testid="device-accessories">
            {#each accessories as acc, i (acc.serial ?? i)}
              <li>
                <span class="jd-mono">{acc.serial ?? '(no serial)'}</span>
                — {acc.accessory_kind ?? 'unknown'}
                {#if acc.installed_on}
                  · installed {acc.installed_on}
                {/if}
              </li>
            {/each}
          </ul>
        {/if}
      </Section>

      <Section title={`Open service jobs (${openJobs.length})`}>
        {#if openJobs.length === 0}
          <p class="empty">No open jobs on this device.</p>
        {:else}
          <ul>
            {#each openJobs as j (j.id)}
              <li>
                <a
                  href={entityHref('job', j.id)}
                  onclick={(e) => {
                    e.preventDefault();
                    navigate(entityHref('job', j.id));
                  }}
                  class="mono"
                >
                  {shortId(j.id)}
                </a>
                — {j.kind} · {j.status}: {j.title}
              </li>
            {/each}
          </ul>
        {/if}
      </Section>

      <Section title={`Event log (${events.length})`} wide>
        {#if events.length === 0}
          <p class="empty">No events for this device yet.</p>
        {:else}
          <table class="data-table">
            <thead>
              <tr>
                <th>Date</th>
                <th>Kind</th>
                <th>Actor</th>
              </tr>
            </thead>
            <tbody>
              {#each events as e (e.id)}
                <tr>
                  <td>{e.ts}</td>
                  <td>{e.kind}</td>
                  <td class="mono">{formatActor(e.actor_id)}</td>
                </tr>
              {/each}
            </tbody>
          </table>
        {/if}
      </Section>
    </div>
  </div>
{/if}
