<script lang="ts">
  // CTO dashboard panel — ML platform. Port of
  // apps/web/src/cto/MlModelsPanel.tsx.

  import Section from '@boss/web-kit/ui/Section.svelte';
  import StatusChip from '@boss/web-kit/ui/StatusChip.svelte';

  type ModelStatus = 'draft' | 'active' | 'shadow' | 'retired';
  type ModelKind =
    | 'risk-score'
    | 'regression'
    | 'classification'
    | 'anomaly-detection'
    | 'ranking';
  type MlModelSummary = {
    id: string;
    name: string;
    kind: ModelKind;
    version: string;
    status: ModelStatus;
    accuracy: number | null;
    accuracy_metric: string | null;
    training_data_ref: string | null;
    description: string | null;
    created_at: string;
    updated_at: string;
    predictions_24h: number;
    latest_prediction_at: string | null;
  };

  type LoadState =
    | { kind: 'loading' }
    | { kind: 'error'; message: string }
    | { kind: 'ready'; models: ReadonlyArray<MlModelSummary> };

  let loadState: LoadState = $state<LoadState>({ kind: 'loading' });

  $effect(() => {
    let cancelled = false;
    (async () => {
      try {
        const r = await fetch('/api/ml/models');
        if (!r.ok) throw new Error(`ml API ${r.status}`);
        const models = (await r.json()) as MlModelSummary[];
        if (!cancelled) loadState = { kind: 'ready', models };
      } catch (e) {
        if (!cancelled) {
          loadState = {
            kind: 'error',
            message: e instanceof Error ? e.message : String(e),
          };
        }
      }
    })();
    return () => {
      cancelled = true;
    };
  });

  // active = serving → ok; retired → muted; draft and shadow → warn
  // (both are versions that exist but are not the one answering, which
  // is what an operator scanning the panel needs to notice).
  function statusTone(status: ModelStatus): 'ok' | 'warn' | 'muted' {
    if (status === 'active') return 'ok';
    if (status === 'retired') return 'muted';
    return 'warn';
  }
  function formatAccuracy(value: number, metric: string | null): string {
    if (metric && metric.toLowerCase().startsWith('rmse')) return value.toFixed(1);
    if (value >= 0 && value <= 1) return `${(value * 100).toFixed(1)}%`;
    return value.toFixed(2);
  }
</script>

<Section title="AI Models" wide>
    {#if loadState.kind === 'loading'}
      <p class="empty">Loading ML platform…</p>
    {:else if loadState.kind === 'error'}
      <p class="empty">ML platform unavailable: {loadState.message}</p>
    {:else}
      {@const models = loadState.models}
      {@const totalPredictions = models.reduce((s, m) => s + m.predictions_24h, 0)}
      {@const activeCount = models.filter((m) => m.status === 'active').length}
      {@const headline =
        models.length === 0
          ? 'No models registered'
          : `${models.length} model${models.length === 1 ? '' : 's'} · ${totalPredictions.toLocaleString()} prediction${totalPredictions === 1 ? '' : 's'} in last 24h · ${activeCount} active`}

      <p class="empty" style="margin-bottom:16px">{headline}</p>
      {#if models.length === 0}
        <p class="empty">
          Phase 1 bootstrap seeds three candidate models on service startup.
          If you see "0 models," boss-ml-api probably isn't running — check
          <code>boss status</code>.
        </p>
      {:else}
        <div
          style="display:grid; grid-template-columns:repeat(auto-fill, minmax(280px, 1fr)); gap:12px"
        >
          {#each models as m (m.id)}
            <div
              style="border:1px solid #374151; border-radius:6px; padding:12px; background:#111827"
            >
              <div style="display:flex; justify-content:space-between; margin-bottom:6px">
                <strong style="color:#e5e7eb">{m.name}</strong>
                <StatusChip value={m.status} tone={statusTone(m.status)} />
              </div>
              <dl
                class="kv"
                style="display:grid; grid-template-columns:auto 1fr; row-gap:3px; column-gap:8px; font-size:12px; margin:0; color:#cbd5e1"
              >
                <dt>kind</dt><dd>{m.kind}</dd>
                <dt>version</dt><dd class="mono">{m.version}</dd>
                {#if m.accuracy != null}
                  <dt>accuracy</dt>
                  <dd>
                    {formatAccuracy(m.accuracy, m.accuracy_metric)}
                    {#if m.accuracy_metric}
                      <span style="color:#64748b"> ({m.accuracy_metric})</span>
                    {/if}
                  </dd>
                {/if}
                <dt>24h</dt>
                <dd>{m.predictions_24h.toLocaleString()} predictions</dd>
                <dt>latest</dt>
                <dd>
                  {m.latest_prediction_at
                    ? new Date(m.latest_prediction_at).toLocaleString()
                    : 'never'}
                </dd>
              </dl>
              {#if m.description}
                <p
                  style="font-size:11px; color:#94a3b8; margin-top:8px; margin-bottom:0; line-height:1.4"
                >
                  {m.description}
                </p>
              {/if}
            </div>
          {/each}
        </div>
      {/if}
    {/if}
</Section>
