<script lang="ts">
  // /admin/policy — port of apps/web/src/admin/PolicyPage.tsx.

  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import Section from '@boss/web-kit/ui/Section.svelte';
  import EditPolicyFlyout from './EditPolicyFlyout.svelte';
  import { session } from '@boss/web-kit/session/session.svelte';
  import type { PolicyRule, Scope } from './policyTypes';

  const ACTIONS = [
    'read', 'create', 'update', 'close', 'sign-off', 'delete', 'publish', 'retire',
  ] as const;
  const RESOURCES = [
    'job', 'step', 'account', 'employee', 'invoice', 'agreement',
    'asset', 'shipment', 'part', 'purchase-order', 'policy-rule',
    'workflow', 'step-plugin',
  ] as const;

  function scopeForDisplay(s: Scope): string {
    if (typeof s === 'string') return s;
    return `department:${s.department}`;
  }

  let rules = $state<ReadonlyArray<PolicyRule>>([]);
  let loading = $state(true);
  let error = $state<string | null>(null);
  let selectedRole = $state<string>('ceo');
  let editing = $state<PolicyRule | null>(null);

  async function load(): Promise<void> {
    loading = true;
    try {
      const r = await fetch('/api/policy/rules');
      if (!r.ok) throw new Error(`HTTP ${r.status}`);
      rules = (await r.json()) as PolicyRule[];
      error = null;
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      loading = false;
    }
  }

  $effect(() => {
    void load();
  });

  let roles = $derived(Array.from(new Set(rules.map((r) => r.role))).sort());

  let byRole = $derived.by(() => {
    const m = new Map<string, PolicyRule[]>();
    for (const r of rules) {
      const list = m.get(r.role);
      if (list) list.push(r);
      else m.set(r.role, [r]);
    }
    return m;
  });
  let selected = $derived(byRole.get(selectedRole) ?? []);

  function cell(resource: string, action: string): PolicyRule | undefined {
    return selected.find((r) => r.resource === resource && r.action === action);
  }

  let currentUserId = $derived(
    session.value.kind === 'ready' ? session.value.user.id : '',
  );
</script>

<div class="catalog theme-exec">
  <PageHeader
    eyebrow="Platform · Policy"
    title="Policy rules"
    subtitle={error
      ? // "0 active rules · 0 roles" above the failure line read as an
        // empty policy (sweep c3e4edcc). The counts are unknown.
        'Rule count unknown — the policy read failed'
      : `${rules.length} active rules · ${roles.length} roles`}
  />

  {#if error}
    <!-- The shared failure marker (sweep c3e4edcc). -->
    <p class="empty load-failed" role="alert" style="margin:0 24px">Failed to load rules: {error}</p>
  {/if}

  <div style="padding:0 24px 16px; display:flex; gap:12px; align-items:center">
    <label style="font-size:13px">
      Role:&nbsp;
      <select bind:value={selectedRole} style="padding:4px 8px; font-size:13px">
        {#each roles as r (r)}
          <option value={r}>{r}</option>
        {/each}
      </select>
    </label>
    <button type="button" class="btn" onclick={load} disabled={loading}>
      {loading ? 'Loading…' : 'Refresh'}
    </button>
  </div>

  <Section title={`${selectedRole} — resource × action matrix`} wide>
      <table class="data-table data-table-striped">
        <thead>
          <tr>
            <th>Resource</th>
            {#each ACTIONS as a (a)}
              <th>{a}</th>
            {/each}
          </tr>
        </thead>
        <tbody>
          {#each RESOURCES as resource (resource)}
            <tr>
              <td class="mono">{resource}</td>
              {#each ACTIONS as action (action)}
                {@const rule = cell(resource, action)}
                <td>
                  {#if rule}
                    <button
                      type="button"
                      class="btn"
                      style="padding:2px 8px; font-size:12px"
                      onclick={() => (editing = rule)}
                    >
                      {scopeForDisplay(rule.scope)}
                    </button>
                  {:else}
                    <span style="color:var(--static); font-size:12px">—</span>
                  {/if}
                </td>
              {/each}
            </tr>
          {/each}
        </tbody>
      </table>
  </Section>

  {#if editing}
    <EditPolicyFlyout
      rule={editing}
      changedBy={currentUserId}
      onClose={() => (editing = null)}
      onSaved={() => {
        editing = null;
        void load();
      }}
    />
  {/if}
</div>
