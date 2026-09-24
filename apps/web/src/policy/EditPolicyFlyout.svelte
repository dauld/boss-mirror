<script lang="ts">
  // Policy-rule edit modal. Port of EditFlyout in PolicyPage.tsx.

  import type { PolicyRule, Scope } from './policyTypes';
  import { classesFor } from '@boss/web-kit/session/classes.svelte';

  // Departments come from the Class registry — the canonical
  // tenant-extensible taxonomy. Brewery sees production /
  // packaging / taproom; used-device-shop sees refurb / service.
  // No per-tenant code; one row per department in the registry,
  // and the dropdown picks them up automatically.
  //
  // Scope shapes that don't map to a department (none / self /
  // team / territory / all) are core policy primitives and stay
  // hardcoded here — they're part of the policy model, not
  // tenant data.
  const CORE_SCOPES: Array<{ value: string; label: string }> = [
    { value: 'none', label: 'None — denied' },
    { value: 'self', label: 'Self — own rows' },
    { value: 'team', label: 'Team — self + direct reports' },
    { value: 'territory', label: 'Territory — account team' },
    { value: 'all', label: 'All — org-wide' },
  ];

  let SCOPE_OPTIONS = $derived<Array<{ value: string; label: string }>>([
    ...CORE_SCOPES,
    ...classesFor('employee', 'department').map((c) => ({
      value: `department:${c.code}`,
      label: `Department: ${c.code}`,
    })),
  ]);

  function scopeForDisplay(s: Scope): string {
    if (typeof s === 'string') return s;
    return `department:${s.department}`;
  }

  type Props = {
    rule: PolicyRule;
    changedBy: string;
    onClose: () => void;
    onSaved: () => void;
  };
  let { rule, changedBy, onClose, onSaved }: Props = $props();

  let scope = $state(scopeForDisplay(rule.scope));
  let reason = $state('');
  let saving = $state(false);
  let err = $state<string | null>(null);

  async function save(): Promise<void> {
    if (!reason.trim()) {
      err = 'Reason is required';
      return;
    }
    saving = true;
    err = null;
    try {
      const parsedScope: Scope = scope.startsWith('department:')
        ? { department: scope.slice('department:'.length) }
        : (scope as Scope);
      const body = {
        rule: { ...rule, scope: parsedScope },
        changed_by: changedBy,
      };
      const r = await fetch(`/api/policy/rules/${encodeURIComponent(rule.id)}`, {
        method: 'PUT',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify(body),
      });
      if (!r.ok) throw new Error(`HTTP ${r.status}`);
      onSaved();
    } catch (e) {
      err = e instanceof Error ? e.message : String(e);
      saving = false;
    }
  }
</script>

<div
  role="dialog"
  aria-modal="true"
  tabindex="-1"
  onclick={onClose}
  onkeydown={(e) => {
    if (e.key === 'Escape') onClose();
  }}
  style="position:fixed; inset:0; background:var(--scrim); display:flex; align-items:center; justify-content:center; z-index:100"
>
  <div
    role="document"
    onclick={(e) => e.stopPropagation()}
    onkeydown={(e) => e.stopPropagation()}
    style="background:var(--ink); border-radius:8px; padding:24px; min-width:420px; max-width:600px"
  >
    <h3 style="margin:0 0 12px">
      Edit: <span class="mono">{rule.role}</span> · {rule.resource} · {rule.action}
    </h3>

    <label style="display:block; margin-bottom:12px">
      Scope
      <select bind:value={scope} style="display:block; width:100%; padding:6px; margin-top:4px; font-size:13px">
        {#each SCOPE_OPTIONS as s (s.value)}
          <option value={s.value}>{s.label}</option>
        {/each}
      </select>
    </label>

    <label style="display:block; margin-bottom:12px">
      Reason for change (required — goes to audit log)
      <textarea
        bind:value={reason}
        rows="3"
        style="display:block; width:100%; padding:6px; margin-top:4px; font-size:13px"
      ></textarea>
    </label>

    {#if err}<p style="color:var(--err); font-size:13px">{err}</p>{/if}

    <div style="display:flex; justify-content:flex-end; gap:8px">
      <button type="button" class="wb-btn" onclick={onClose} disabled={saving}>Cancel</button>
      <button
        type="button"
        class="wb-btn wb-btn-primary"
        onclick={save}
        disabled={saving}
      >
        {saving ? 'Saving…' : 'Save rule'}
      </button>
    </div>
  </div>
</div>
