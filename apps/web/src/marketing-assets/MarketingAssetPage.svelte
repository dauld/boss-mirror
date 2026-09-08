<script lang="ts">
  // Marketing Asset detail.

  import Breadcrumb from '@boss/web-kit/ui/Breadcrumb.svelte';
  import { formatDate } from '@boss/web-kit/ui/date';
  import EntityLink from '@boss/web-kit/ui/EntityLink.svelte';
  import Meta from '@boss/web-kit/ui/Meta.svelte';
  import Section from '@boss/web-kit/ui/Section.svelte';
  import { type MarketingAsset } from './types';
  import { loadClasses, classesFor } from '@boss/web-kit/session/classes.svelte';
  import { href } from '../router';

  type Props = { assetId: string };
  let { assetId }: Props = $props();

  let asset = $state<MarketingAsset | null>(null);
  let history = $state<MarketingAsset[]>([]);
  /// Non-null when the record fetch FAILED (5xx / network) — rendered
  /// instead of "Asset not found", which is a claim only a successful
  /// lookup gets to make (packet 3fba9c35).
  let loadFailed = $state<string | null>(null);
  let loading = $state(true);
  let empNames = $state<Map<string, string>>(new Map());

  let decoded = $derived(decodeURIComponent(assetId));

  // Kind label from the Class registry (subject_kind='marketing-asset').
  $effect(() => {
    void loadClasses('marketing-asset');
  });
  let kindLabel = $derived(
    new Map(
      classesFor('marketing-asset', 'kind').map(
        (c): [string, string] => [c.code, c.display_name],
      ),
    ),
  );

  $effect(() => {
    const id = decoded;
    let cancelled = false;
    loading = true;
    (async () => {
      try {
        const [aResp, hResp, pResp] = await Promise.all([
          fetch(`/api/catalog/marketing-assets/${encodeURIComponent(id)}`),
          fetch(`/api/catalog/marketing-assets/${encodeURIComponent(id)}/history`),
          fetch('/api/people'),
        ]);
        if (aResp.status === 404) {
          if (!cancelled) {
            asset = null;
            loadFailed = null;
          }
        } else if (aResp.ok) {
          const body = (await aResp.json()) as MarketingAsset;
          if (!cancelled) {
            asset = body;
            loadFailed = null;
          }
        } else {
          if (!cancelled) {
            asset = null;
            loadFailed = `HTTP ${aResp.status}`;
          }
        }
        if (hResp.ok) {
          const body = (await hResp.json()) as MarketingAsset[];
          if (!cancelled) history = Array.isArray(body) ? body : [];
        }
        if (pResp.ok) {
          const people = (await pResp.json()) as Array<{ id: string; name: string }>;
          const m = new Map<string, string>();
          for (const e of people) m.set(e.id, e.name);
          if (!cancelled) empNames = m;
        }
      } catch (e) {
        if (!cancelled) {
          asset = null;
          loadFailed = e instanceof Error ? e.message : String(e);
        }
      }
      if (!cancelled) loading = false;
    })();
    return () => {
      cancelled = true;
    };
  });

  let retired = $derived(Boolean(asset?.retired_at));
  let hasPredecessor = $derived((asset?.supersedes_id ?? null) !== null);
  let hasSuccessor = $derived.by(() => {
    if (!asset) return false;
    return history.some(
      (h) => h.id !== asset!.id && h.created_at > asset!.created_at,
    );
  });
</script>

<div class="detail-page theme-exec">
  <Breadcrumb to={href('/ux/marketing-assets')}>
    ← Assets
  </Breadcrumb>

  {#if loading && !asset}
    <p class="empty">Loading…</p>
  {:else if loadFailed}
    <p class="empty load-failed" role="alert">
      Couldn't load this asset — {loadFailed}
    </p>
  {:else if !asset}
    <header class="detail-hero">
      <h1 class="detail-title">Asset not found</h1>
    </header>
    <p class="empty">No marketing asset with id <code>{decoded}</code>.</p>
  {:else}
    {@const a = asset}
    <header class="detail-hero">
      <div>
        <div class="detail-eyebrow">
          <EntityLink kind="marketing-asset" id={a.id} /> ·
          {a.kind ? (kindLabel.get(a.kind) ?? a.kind) : '—'}
          {#if retired}
            <span class="chip" style="margin-left:8px">RETIRED</span>
          {/if}
          {#if hasSuccessor && !retired}
            <span class="chip chip-warn" style="margin-left:8px">SUPERSEDED</span>
          {/if}
        </div>
        <h1 class="detail-title">{a.title}</h1>
        {#if a.description}
          <div class="detail-tagline">{a.description}</div>
        {/if}
        <div class="detail-meta">
          <Meta label="Tags">{a.tags.length}</Meta>
          <Meta label="Links">
              {a.linked_device_skus.length + a.linked_account_ids.length + a.linked_campaign_ids.length}
          </Meta>
          <Meta label="Versions">{history.length || 1}</Meta>
          <Meta label="Updated">{formatDate(a.updated_at)}</Meta>
        </div>
      </div>
    </header>

    <div class="tab-grid">
      <Section title="Profile">
          <dl class="kv">
            <dt>File</dt>
            <dd>
              {#if a.file_url}
                <a href={a.file_url} target="_blank" rel="noopener noreferrer">{a.file_url}</a>
              {:else}
                —
              {/if}
            </dd>
            <dt>Owner</dt>
            <dd>
              {#if a.owner_id}
                <EntityLink
                  kind="employee"
                  id={a.owner_id}
                  label={empNames.get(a.owner_id)}
                />
              {:else}
                —
              {/if}
            </dd>
            <dt>Brand-reviewed</dt>
            <dd>
              {#if a.brand_reviewed_by}
                <EntityLink
                  kind="employee"
                  id={a.brand_reviewed_by}
                  label={empNames.get(a.brand_reviewed_by)}
                />
                {#if a.brand_reviewed_at}
                  <span style="color:var(--static); margin-left:8px">
                    · {formatDate(a.brand_reviewed_at)}
                  </span>
                {/if}
              {:else}
                <span style="color:var(--static)">not reviewed</span>
              {/if}
            </dd>
            <dt>Created</dt><dd>{formatDate(a.created_at)}</dd>
            <dt>Supersedes</dt>
            <dd>
              {#if hasPredecessor && a.supersedes_id}
                <a href={href(`/ux/marketing-assets/${encodeURIComponent(a.supersedes_id)}`)}>
                  {a.supersedes_id}
                </a>
              {:else}
                <span style="color:var(--static)">(original)</span>
              {/if}
            </dd>
          </dl>
      </Section>

      <Section title={`Tags (${a.tags.length})`}>
          {#if a.tags.length === 0}
            <p class="empty">No tags yet.</p>
          {:else}
            <div class="chips">
              {#each a.tags as t (t)}
                <span class="chip">{t}</span>
              {/each}
            </div>
          {/if}
      </Section>

      <Section title="Linked entities" wide>
          <div style="display:grid; grid-template-columns:1fr 1fr 1fr; gap:16px">
            <div>
              <h4 style="font-size:12px; color:var(--static); margin-bottom:6px">
                Device SKUs ({a.linked_device_skus.length})
              </h4>
              {#if a.linked_device_skus.length === 0}
                <p class="empty" style="margin:0">None.</p>
              {:else}
                <ul style="list-style:none; padding:0; margin:0; font-size:13px">
                  {#each a.linked_device_skus as id (id)}
                    {@const path = id.startsWith('FP-') ? `/ux/products/${encodeURIComponent(id)}` : `/ux/catalog/${encodeURIComponent(id)}`}
                    <li style="margin-bottom:4px">
                      <a href={href(path)}>{id}</a>
                    </li>
                  {/each}
                </ul>
              {/if}
            </div>
            <div>
              <h4 style="font-size:12px; color:var(--static); margin-bottom:6px">
                Accounts ({a.linked_account_ids.length})
              </h4>
              {#if a.linked_account_ids.length === 0}
                <p class="empty" style="margin:0">None.</p>
              {:else}
                <ul style="list-style:none; padding:0; margin:0; font-size:13px">
                  {#each a.linked_account_ids as id (id)}
                    <li style="margin-bottom:4px">
                      <EntityLink kind="account" id={id} />
                    </li>
                  {/each}
                </ul>
              {/if}
            </div>
            <div>
              <h4 style="font-size:12px; color:var(--static); margin-bottom:6px">
                Campaigns ({a.linked_campaign_ids.length})
              </h4>
              {#if a.linked_campaign_ids.length === 0}
                <p class="empty" style="margin:0">None.</p>
              {:else}
                <ul style="list-style:none; padding:0; margin:0; font-size:13px">
                  {#each a.linked_campaign_ids as id (id)}
                    <li style="margin-bottom:4px">
                      <a
                        href={href(
                          `/ux/jobs?subject_kind=campaign&subject_id=${encodeURIComponent(id)}`,
                        )}
                      >
                        {id}
                      </a>
                    </li>
                  {/each}
                </ul>
              {/if}
            </div>
          </div>
      </Section>
    </div>

    <Section title={`Version history (${history.length || 1})`} wide>
        {#if history.length <= 1}
          <p class="empty">
            No prior versions — this is the original.
            {#if hasSuccessor}
              (A newer version supersedes it — see the chain when viewing the successor.)
            {/if}
          </p>
        {:else}
          <table class="data-table">
            <thead>
              <tr>
                <th>Version</th>
                <th>ID</th>
                <th>Title</th>
                <th>Created</th>
                <th>Status</th>
              </tr>
            </thead>
            <tbody>
              {#each history as h, i (h.id)}
                <tr style={`font-weight:${h.id === a.id ? 600 : 400}`}>
                  <td>v{i + 1}</td>
                  <td class="mono">
                    {#if h.id === a.id}
                      {h.id}
                    {:else}
                      <a href={href(`/ux/marketing-assets/${encodeURIComponent(h.id)}`)}>{h.id}</a>
                    {/if}
                  </td>
                  <td>{h.title}</td>
                  <td>{formatDate(h.created_at)}</td>
                  <td>
                    {#if h.retired_at}
                      retired
                    {:else if i === history.length - 1}
                      current
                    {:else}
                      superseded
                    {/if}
                  </td>
                </tr>
              {/each}
            </tbody>
          </table>
        {/if}
    </Section>

    <Section title="Insights" wide>
        <p class="empty">
          Download count, campaigns used in, and motion references will land
          with the attribution plugin once it grows past read-only mode.
        </p>
    </Section>

    <Section title="In-flight motions" wide>
        <p class="empty">
          Active <code>marketing-motion</code> Jobs referencing this asset
          via their tier 3 checklist step will surface here once session 1's
          motion execution picks up assets in metadata.
        </p>
    </Section>
  {/if}
</div>
