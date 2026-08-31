<script lang="ts">
  // /it/registry/rules/{name} (and {name}==='new' for create mode) —
  // edit a dispatcher rule: a draft form seeded from the active/latest
  // version, a version-history table, and the draft → publish/retire
  // lifecycle actions. Models the step-plugin detail page (LoadState
  // discriminated union + action/actionError pattern + version-history
  // table). Writes flow through ./ruleAuthoring.

  import Breadcrumb from '@boss/web-kit/ui/Breadcrumb.svelte';
  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import Section from '@boss/web-kit/ui/Section.svelte';
  import {
    listVersions,
    createDraft,
    validateRule,
    publishRule,
    retireRule,
    buildRuleSpec,
    type RuleVersion,
    type RuleStatus,
    type RuleSpec,
  } from './ruleAuthoring';
  import { href, navigate } from '../router';

  type Props = { ruleName: string };
  let { ruleName }: Props = $props();

  let isNew = $derived(ruleName === 'new');

  type LoadState =
    | { kind: 'loading' }
    | { kind: 'error'; message: string }
    | { kind: 'ready'; versions: ReadonlyArray<RuleVersion> };

  let loadState = $state<LoadState>({ kind: 'loading' });

  // --- Editable form model ---------------------------------------------
  // `do` args are edited as an ordered list of key/value rows (rather than
  // free-form JSON) so the shape stays typed end-to-end. An empty-key row
  // is dropped on build.
  type ArgRow = { key: string; value: string };
  type DoRow = { handler: string; args: ArgRow[] };

  let formName = $state('');
  let onEvent = $state('');
  let whenExpr = $state('');
  let delay = $state('');
  let doRows = $state<DoRow[]>([{ handler: '', args: [] }]);

  // --- Action / validation feedback ------------------------------------
  let action = $state<string | null>(null);
  let actionError = $state<string | null>(null);
  let validateState = $state<{ ok: boolean; error: string | null } | null>(null);

  function seedFrom(v: RuleVersion): void {
    formName = v.name;
    onEvent = v.on_event;
    whenExpr = v.when ?? '';
    delay = v.delay ?? '';
    doRows = v.do.map((d) => ({
      handler: d.handler,
      args: Object.entries(d.args).map(([key, value]) => ({ key, value })),
    }));
    if (doRows.length === 0) doRows = [{ handler: '', args: [] }];
  }

  async function load(): Promise<void> {
    validateState = null;
    actionError = null;
    if (isNew) {
      formName = '';
      onEvent = '';
      whenExpr = '';
      delay = '';
      doRows = [{ handler: '', args: [] }];
      loadState = { kind: 'ready', versions: [] };
      return;
    }
    try {
      const versions = await listVersions(ruleName);
      if (versions.length === 0) {
        loadState = { kind: 'error', message: `No versions found for "${ruleName}".` };
        return;
      }
      // Seed from the active version, else the latest (versions are
      // oldest-first, so the last entry is newest).
      const seed = versions.find((v) => v.status === 'active') ?? versions[versions.length - 1]!;
      seedFrom(seed);
      loadState = { kind: 'ready', versions };
    } catch (e) {
      loadState = { kind: 'error', message: e instanceof Error ? e.message : String(e) };
    }
  }

  $effect(() => {
    void ruleName;
    void load();
  });

  // --- do-row / arg-row editing ----------------------------------------
  function addDoRow(): void {
    doRows = [...doRows, { handler: '', args: [] }];
  }
  function removeDoRow(i: number): void {
    doRows = doRows.filter((_, idx) => idx !== i);
    if (doRows.length === 0) doRows = [{ handler: '', args: [] }];
  }
  function addArg(i: number): void {
    doRows = doRows.map((row, idx) =>
      idx === i ? { ...row, args: [...row.args, { key: '', value: '' }] } : row,
    );
  }
  function removeArg(i: number, ai: number): void {
    doRows = doRows.map((row, idx) =>
      idx === i ? { ...row, args: row.args.filter((_, j) => j !== ai) } : row,
    );
  }

  /** Build the API spec from the editor's $state via the shared, tested
   *  normalizer — so Validate and Save send byte-identical specs. */
  function buildSpec(): RuleSpec {
    return buildRuleSpec({
      name: formName,
      on_event: onEvent,
      when: whenExpr,
      delay,
      do: doRows,
    });
  }

  function statusChipClass(status: RuleStatus): string {
    return status === 'active' ? 'ok' : status === 'retired' ? 'muted' : 'warn';
  }

  async function runValidate(): Promise<void> {
    action = 'validate';
    actionError = null;
    try {
      validateState = await validateRule(buildSpec());
    } catch (e) {
      actionError = e instanceof Error ? e.message : String(e);
    } finally {
      action = null;
    }
  }

  async function runSaveDraft(): Promise<void> {
    action = 'save';
    actionError = null;
    validateState = null;
    const spec = buildSpec();
    if (spec.name.length === 0) {
      actionError = 'Rule name is required.';
      action = null;
      return;
    }
    if (spec.on_event.length === 0) {
      actionError = 'on_event is required.';
      action = null;
      return;
    }
    try {
      const created = await createDraft(spec);
      if (isNew) {
        // Land on the now-existing rule's editor.
        navigate(href(`/it/registry/rules/${encodeURIComponent(created.name)}`));
        return;
      }
      await load();
    } catch (e) {
      actionError = e instanceof Error ? e.message : String(e);
    } finally {
      action = null;
    }
  }

  async function runPublish(): Promise<void> {
    action = 'publish';
    actionError = null;
    try {
      await publishRule(ruleName);
      await load();
    } catch (e) {
      actionError = e instanceof Error ? e.message : String(e);
    } finally {
      action = null;
    }
  }

  async function runRetire(): Promise<void> {
    action = 'retire';
    actionError = null;
    try {
      const msg =
        `Retire rule "${ruleName}"?\n\n` +
        `The active version flips to retired. In-flight events already ` +
        `matched are unaffected; on the next dispatcher restart this rule ` +
        `stops firing.`;
      if (!window.confirm(msg)) {
        action = null;
        return;
      }
      await retireRule(ruleName);
      await load();
    } catch (e) {
      actionError = e instanceof Error ? e.message : String(e);
    } finally {
      action = null;
    }
  }
</script>

{#if loadState.kind === 'loading'}
  <div class="catalog theme-exec">
    <p class="empty">Loading…</p>
  </div>
{:else if loadState.kind === 'error'}
  <div class="catalog theme-exec">
    <Breadcrumb to={href('/it/registry/rules')}>← All dispatcher rules</Breadcrumb>
    <PageHeader eyebrow="Platform · Dispatcher rule" title={ruleName} subtitle={loadState.message} />
  </div>
{:else}
  {@const versions = loadState.versions}
  {@const hasDraft = versions.some((v) => v.status === 'draft')}
  {@const hasActive = versions.some((v) => v.status === 'active')}
  {@const active = versions.find((v) => v.status === 'active')}
  <div class="catalog theme-exec">
    <Breadcrumb to={href('/it/registry/rules')}>← All dispatcher rules</Breadcrumb>
    <PageHeader
      eyebrow="Platform · Dispatcher rule"
      title={isNew ? 'New dispatcher rule' : ruleName}
      subtitle={isNew
        ? 'Author a rule, then Save draft. Publishing activates it (retiring the prior active version).'
        : `${versions.length} version${versions.length === 1 ? '' : 's'}${active ? ` · active v${active.version}` : ' · no active version'}`}
    />

    <p class="empty" style="padding:0 24px 8px; color:#92400e">
      Published changes take effect on the next dispatcher restart — live
      hot-reload is a follow-up.
    </p>

    <!-- Lifecycle actions -->
    <div style="padding:0 24px 16px; display:flex; gap:12px; align-items:center; flex-wrap:wrap">
      <button
        type="button"
        class="wb-btn"
        onclick={runValidate}
        disabled={action !== null}
        title="Dry-run the draft against the dispatcher parser"
      >
        {action === 'validate' ? 'Validating…' : 'Validate'}
      </button>
      <button
        type="button"
        class="wb-btn wb-btn-primary"
        onclick={runSaveDraft}
        disabled={action !== null}
        title="Persist a new draft version (validated server-side)"
      >
        {action === 'save' ? 'Saving…' : 'Save draft'}
      </button>
      {#if !isNew}
        <button
          type="button"
          class="wb-btn"
          onclick={runPublish}
          disabled={!hasDraft || action !== null}
          title={hasDraft ? 'Activate the latest draft' : 'No draft to publish'}
        >
          {action === 'publish' ? 'Publishing…' : 'Publish draft'}
        </button>
        <button
          type="button"
          class="wb-btn"
          onclick={runRetire}
          disabled={!hasActive || action !== null}
          title={hasActive ? 'Retire the active version' : 'No active version to retire'}
        >
          {action === 'retire' ? 'Retiring…' : 'Retire'}
        </button>
      {/if}
      {#if validateState}
        {#if validateState.ok}
          <span style="color:#166534; font-size:13px">✓ Valid</span>
        {:else}
          <span style="color:#dc2626; font-size:13px">✗ {validateState.error}</span>
        {/if}
      {/if}
      {#if actionError}
        <span style="color:#dc2626; font-size:13px">{actionError}</span>
      {/if}
    </div>

    <div class="tab-grid">
      <!-- Editor form -->
      <Section title="Rule">
        <div style="display:grid; gap:12px; max-width:800px">
          <div>
            <div style="font-size:12px; color:#666; margin-bottom:2px">
              Name
              {#if !isNew}<span style="color:#888"> — the rule's permanent identity (not editable)</span>{/if}
            </div>
            <input
              bind:value={formName}
              readonly={!isNew}
              placeholder="advance-dag-on-step-done"
              class="mono"
              style="padding:6px; font-size:13px; width:100%"
            />
          </div>
          <div>
            <div style="font-size:12px; color:#666; margin-bottom:2px">
              On event <span style="color:#888"> — the NATS topic this rule listens for</span>
            </div>
            <input
              bind:value={onEvent}
              placeholder="step.done.*"
              class="mono"
              style="padding:6px; font-size:13px; width:100%"
            />
          </div>
          <div>
            <div style="font-size:12px; color:#666; margin-bottom:2px">
              When <span style="color:#888"> — optional predicate; rule fires only when it's true</span>
            </div>
            <input
              bind:value={whenExpr}
              placeholder="event.kind == &quot;billing&quot;"
              class="mono"
              style="padding:6px; font-size:13px; width:100%"
            />
          </div>
          <div>
            <div style="font-size:12px; color:#666; margin-bottom:2px">
              Delay <span style="color:#888"> — optional; defers the side-effects (e.g. 5m, 1h)</span>
            </div>
            <input
              bind:value={delay}
              placeholder=""
              class="mono"
              style="padding:6px; font-size:13px; width:240px"
            />
          </div>
        </div>
      </Section>

      <!-- do steps -->
      <Section title="Do steps" wide>
        <p class="empty" style="padding:0 0 8px; text-align:left">
          Each step runs a handler with concrete args (string expressions
          evaluated against the event). Steps run in order.
        </p>
        <div style="display:grid; gap:16px; max-width:900px">
          {#each doRows as row, i (i)}
            <div style="border:1px solid #e7e5e4; border-radius:6px; padding:12px">
              <div style="display:flex; gap:8px; align-items:center; margin-bottom:8px">
                <span style="font-size:12px; color:#888; width:48px">#{i + 1}</span>
                <input
                  bind:value={row.handler}
                  placeholder="handler-name"
                  class="mono"
                  style="padding:6px; font-size:13px; flex:1"
                />
                <button
                  type="button"
                  class="wb-btn"
                  onclick={() => removeDoRow(i)}
                  title="Remove this step"
                >
                  Remove
                </button>
              </div>
              <div style="padding-left:56px; display:grid; gap:6px">
                {#each row.args as arg, ai (ai)}
                  <div style="display:flex; gap:8px; align-items:center">
                    <input
                      bind:value={arg.key}
                      placeholder="arg"
                      class="mono"
                      style="padding:5px; font-size:12px; width:200px"
                    />
                    <span style="color:#888">=</span>
                    <input
                      bind:value={arg.value}
                      placeholder="expression"
                      class="mono"
                      style="padding:5px; font-size:12px; flex:1"
                    />
                    <button
                      type="button"
                      class="wb-btn"
                      onclick={() => removeArg(i, ai)}
                      title="Remove this arg"
                    >
                      ✕
                    </button>
                  </div>
                {/each}
                <div>
                  <button type="button" class="wb-btn" onclick={() => addArg(i)}>+ arg</button>
                </div>
              </div>
            </div>
          {/each}
          <div>
            <button type="button" class="wb-btn" onclick={addDoRow}>+ do step</button>
          </div>
        </div>
      </Section>

      <!-- Version history -->
      {#if !isNew}
        <Section title={`Version history (${versions.length})`}>
          <table class="data-table data-table-striped">
            <thead>
              <tr>
                <th class="num">Version</th>
                <th>Status</th>
                <th>Created</th>
              </tr>
            </thead>
            <tbody>
              {#each versions as v (v.version)}
                <tr>
                  <td class="num">{v.version}</td>
                  <td>
                    <span class="chip chip-stage chip-stage-{statusChipClass(v.status)}">
                      {v.status}
                    </span>
                  </td>
                  <td>{new Date(v.created_at).toISOString().slice(0, 19).replace('T', ' ')}</td>
                </tr>
              {/each}
            </tbody>
          </table>
        </Section>
      {/if}
    </div>
  </div>
{/if}
