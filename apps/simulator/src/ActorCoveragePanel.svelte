<script lang="ts">
  // Actor coverage — is the sim driving the WHOLE roster? Renders the
  // telemetry's `actor_coverage` block: headline totals, the dormant
  // roles named prominently, and the full per-role table. The point is
  // that an under-simulated brewery is VISIBLE on the cockpit's face
  // instead of being rediscovered by SQL. Ordering/labeling logic lives
  // in ./actor-coverage (unit-tested); this component only renders.
  import Section from '@boss/web-kit/ui/Section.svelte';
  import StatusChip from '@boss/web-kit/ui/StatusChip.svelte';
  import { dormantRoles, sortRoles, statusLabel } from './actor-coverage';
  import type { ActorCoverage } from './types';

  let { coverage }: Readonly<{ coverage: ActorCoverage | undefined }> = $props();

  let rows = $derived(coverage ? sortRoles(coverage.roles) : []);
  let dormant = $derived(coverage ? dormantRoles(coverage.roles) : []);
  // Simulatable = total minus the by-design operator exclusions; the
  // denominator dormancy is judged against.
  let simRoles = $derived(coverage ? coverage.roles_total - coverage.roles_operator : 0);
  let simEmployees = $derived(
    coverage ? coverage.employees_total - coverage.employees_operator : 0,
  );
</script>

<Section title="Actor coverage" wide>
  <p class="point-sub">
    Who on the roster has actually acted — distinct employees per role that completed ≥ 1 step
    this daemon lifetime. A dormant role means no live workflow step routes work to it.
  </p>
  {#if !coverage}
    <p class="status">No coverage telemetry yet (daemon predates actor coverage).</p>
  {:else}
    <div class="cov-headline">
      <div class="stat">
        <span class="stat-num">{coverage.roles_acting}</span>
        <span class="stat-label">of {simRoles} roles acting</span>
      </div>
      <div class="stat">
        <span class="stat-num">{coverage.employees_acting.toLocaleString()}</span>
        <span class="stat-label">of {simEmployees.toLocaleString()} employees acting</span>
      </div>
      <div class="stat" class:alarm={coverage.roles_dormant > 0}>
        <span class="stat-num">{coverage.roles_dormant}</span>
        <span class="stat-label">dormant roles</span>
      </div>
    </div>

    {#if dormant.length > 0}
      <div class="dormant-strip">
        <span class="dormant-title">Dormant — never completed a step:</span>
        {#each dormant as r (r)}
          <span class="dormant-role">{r}</span>
        {/each}
      </div>
    {/if}

    <table class="data-table">
      <thead>
        <tr>
          <th class="t-role">Role</th>
          <th class="t-num">Roster</th>
          <th class="t-num">Acting</th>
          <th class="t-num">Completions</th>
          <th class="t-status"></th>
        </tr>
      </thead>
      <tbody>
        {#each rows as r (r.role)}
          <tr class:is-dormant={r.status === 'dormant'} class:is-operator={r.status === 'operator'}>
            <td class="t-role">{r.role}</td>
            <td class="t-num">{r.roster.toLocaleString()}</td>
            <td class="t-num">{r.acting.toLocaleString()}</td>
            <td class="t-num">{r.completions.toLocaleString()}</td>
            <td class="t-status">
              <!-- Dormant is a state, so it is the troubled plate; an
                   operator-held role is a deliberate exclusion, not a
                   state, so it wears the quiet frame. -->
              {#if r.status !== 'acting'}
                <StatusChip
                  value={statusLabel(r.status)}
                  tone={r.status === 'dormant' ? 'err' : 'muted'}
                />
              {/if}
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
    {#if coverage.roles_operator > 0}
      <p class="cov-foot">
        Operator-held roles are excluded from sim driving by design — their steps wait for the
        real human.
      </p>
    {/if}
  {/if}
</Section>

<style>
  /* Enamel (backlog 6f471ff6, car 4): every colour is a token from the
     web app's stylesheet, which the Simulator imports. The table is the
     shared .data-table — ink band heads, comfortable rows — and a role's
     state is a StatusChip plate. Dormant is troubled, and troubled is the
     loudest object on the page, so the strip naming the dormant roles
     wears the failed read's rail. */
  .point-sub {
    margin: 0 0 10px;
    font-size: 12.5px;
    color: var(--static);
  }
  .cov-headline {
    display: flex;
    flex-wrap: wrap;
    gap: 8px 28px;
    margin-bottom: 12px;
  }
  .stat {
    display: flex;
    align-items: baseline;
    gap: 8px;
  }
  /* The board's summary figure: mono, 24px, 600, tabular. */
  .stat-num {
    font-family: var(--font-mono);
    font-size: 24px;
    font-weight: 600;
    font-variant-numeric: tabular-nums;
    color: var(--fog);
    line-height: 1;
  }
  .stat-label {
    font-family: var(--font-mono);
    font-size: 11px;
    text-transform: uppercase;
    letter-spacing: var(--ls-label);
    color: var(--static);
  }
  .stat.alarm .stat-num,
  .stat.alarm .stat-label {
    color: var(--troubled-ink);
  }
  .dormant-strip {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 6px;
    padding: 12px 14px;
    margin-bottom: 12px;
    background: var(--card);
    border-left: 8px solid var(--troubled);
    border-radius: var(--radius);
  }
  .dormant-title {
    font-weight: 600;
    color: var(--troubled-ink);
  }
  .dormant-role {
    font-family: var(--font-mono);
    font-size: 12px;
    color: var(--troubled-ink);
    background: var(--err-wash);
    padding: 0 0.45em;
    border-radius: var(--radius);
  }
  .t-role {
    font-weight: 600;
  }
  .t-num {
    text-align: right;
    font-variant-numeric: tabular-nums;
  }
  .t-status {
    text-align: right;
    white-space: nowrap;
  }
  tr.is-dormant .t-role,
  tr.is-dormant .t-num {
    color: var(--troubled-ink);
  }
  tr.is-operator .t-role,
  tr.is-operator .t-num {
    color: var(--static);
    font-weight: 400;
  }
  .cov-foot {
    margin: 8px 0 0;
    font-size: 12.5px;
    color: var(--static);
  }
  .status {
    margin: 0 0 6px;
    color: var(--static);
  }
</style>
