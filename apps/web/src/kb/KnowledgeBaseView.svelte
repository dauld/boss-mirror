<script lang="ts">
  // Polymorphic KB view — four sections driven by a Subject identity:
  //
  //   1. Timeline — chronological facts pulled from the Subject's
  //      event log (accounts: /api/people/accounts/{id}/facts;
  //      systems: the `events` array on /api/assets/{serial}).
  //   2. Active jobs — open/blocked Jobs referencing this Subject.
  //   3. Completed jobs — closed Jobs referencing this Subject, top 10.
  //   4. Documents — catalog documents with this entity as the target.
  //
  // Usage: `<KnowledgeBaseView entityKind="account" entityId="p-1" />`.
  //
  // Polymorphism today covers account + system (the two that have
  // event-log endpoints). Extending to vendor / employee / campaign
  // is a data-side question — the component picks up a new kind as
  // soon as a fact / job / documents endpoint lands.

  import Section from '@boss/web-kit/ui/Section.svelte';
  import Link from '@boss/web-kit/ui/Link.svelte';
  import { href } from '../router';
  import { entityHref } from '@boss/web-kit/ui/entity-href';
  import { formatActor } from '../data/actor';
  import { relatedJobsUrl } from './relatedJobs';
  import { loadOwnerNames, personIdsOf } from '../data/ownerNames';
  import { okRead, type ReadState } from '../data/readState';

  type EntityKind = 'account' | 'asset';

  type Fact = {
    id?: string;
    fact_kind?: string;
    kind?: string;
    occurred_at?: string;
    ts?: string;
    actor_id?: string | null;
    job_id?: string | null;
    payload?: Record<string, unknown>;
  };

  type Job = {
    id: string;
    kind: string;
    title: string;
    status: string;
    priority: string;
    owner_id: string;
    opened_on: string;
  };

  type KBDocument = {
    id: string;
    doc_type: string;
    title: string;
    url: string | null;
    version: string | null;
    audience: string;
    uploaded_at: string | null;
  };

  type Props = {
    entityKind: EntityKind;
    entityId: string;
  };
  let { entityKind, entityId }: Props = $props();

  let facts = $state<Fact[]>([]);
  let jobs = $state<Job[]>([]);
  let docs = $state<KBDocument[]>([]);
  /// Non-null when the timeline (facts) fetch failed — rendered
  /// instead of "No recorded activity yet", which is a claim only a
  /// successful read gets to make (packet 3fba9c35). Jobs/documents
  /// sections normally hide when empty, so a failed fetch gets its
  /// own visible notice instead of a silent absence.
  let factsFailed = $state<string | null>(null);
  let jobsFailed = $state<string | null>(null);
  let docsFailed = $state<string | null>(null);
  let empNames = $state<ReadonlyMap<string, string>>(new Map());
  let namesRead = $state<ReadState>(okRead);
  let workflowLabels = $state<Map<string, string>>(new Map());

  // --- Facts fetch ---------------------------------------------------------
  $effect(() => {
    const kind = entityKind;
    const id = entityId;
    if (!id) return;
    let cancelled = false;
    (async () => {
      const url =
        kind === 'account'
          ? `/api/people/accounts/${encodeURIComponent(id)}/facts`
          : kind === 'asset'
            ? `/api/assets/${encodeURIComponent(id)}`
            : null;
      if (!url) return;
      try {
        const r = await fetch(url);
        if (cancelled) return;
        if (!r.ok) {
          factsFailed = `HTTP ${r.status}`;
          return;
        }
        const data = await r.json();
        const normalized = Array.isArray(data)
          ? (data as Fact[])
          : Array.isArray(data?.events)
            ? (data.events as Fact[])
            : Array.isArray(data?.data)
              ? (data.data as Fact[])
              : [];
        if (!cancelled) {
          facts = normalized;
          factsFailed = null;
        }
      } catch (e) {
        if (!cancelled) factsFailed = e instanceof Error ? e.message : String(e);
      }
    })();
    return () => {
      cancelled = true;
    };
  });

  // --- Related Jobs fetch --------------------------------------------------
  $effect(() => {
    const id = entityId;
    if (!id) return;
    let cancelled = false;
    (async () => {
      try {
        const r = await fetch(relatedJobsUrl(id));
        if (cancelled) return;
        if (!r.ok) {
          jobsFailed = `HTTP ${r.status}`;
          return;
        }
        const data = await r.json();
        const rows: Job[] = Array.isArray(data?.data) ? data.data : [];
        if (!cancelled) {
          jobs = rows;
          jobsFailed = null;
        }
      } catch (e) {
        if (!cancelled) jobsFailed = e instanceof Error ? e.message : String(e);
      }
    })();
    return () => {
      cancelled = true;
    };
  });

  // --- Documents fetch -----------------------------------------------------
  $effect(() => {
    const kind = entityKind;
    const id = entityId;
    if (!id) return;
    let cancelled = false;
    (async () => {
      try {
        const r = await fetch(
          `/api/catalog/documents?entity_kind=${encodeURIComponent(kind)}&entity_id=${encodeURIComponent(id)}`,
        );
        if (cancelled) return;
        if (!r.ok) {
          docsFailed = `HTTP ${r.status}`;
          return;
        }
        const data = await r.json();
        if (!cancelled) {
          docs = Array.isArray(data) ? (data as KBDocument[]) : [];
          docsFailed = null;
        }
      } catch (e) {
        if (!cancelled) docsFailed = e instanceof Error ? e.message : String(e);
      }
    })();
    return () => {
      cancelled = true;
    };
  });

  // --- Employee-name lookup (for timeline actor labels) -------------------
  // Only the people acting on the facts shown (the first 50), one row
  // each, and a failure is said above the timeline. Until backlog
  // 1e73bd93 this read the WHOLE roster and dropped a refusal or a
  // network error, so the actors silently became ids. Machine actors
  // are never asked about (see ../data/ownerNames.ts).
  let actorKey = $derived(personIdsOf(facts.slice(0, 50).map((f) => f.actor_id)).join('\n'));
  $effect(() => {
    const ids = actorKey ? actorKey.split('\n') : [];
    let cancelled = false;
    (async () => {
      const out = await loadOwnerNames(ids);
      if (!cancelled) {
        empNames = out.names;
        namesRead = out.read;
      }
    })();
    return () => {
      cancelled = true;
    };
  });

  // --- Job-kind label lookup (for job rows) --------------------------------
  $effect(() => {
    let cancelled = false;
    (async () => {
      try {
        const r = await fetch('/api/workflows');
        if (!r.ok || cancelled) return;
        const rows = (await r.json()) as Array<{ kind: string; label: string }>;
        if (!cancelled) {
          workflowLabels = new Map(rows.map((k) => [k.kind, k.label]));
        }
      } catch {
        /* ignore */
      }
    })();
    return () => {
      cancelled = true;
    };
  });

  let openJobs = $derived(
    jobs.filter((j) => j.status === 'open' || j.status === 'blocked'),
  );
  let closedJobs = $derived(jobs.filter((j) => j.status === 'closed'));

  const FACT_ICON: Record<string, string> = {
    'opportunity-opened': '💰',
    'service-request': '🔧',
    'service-ticket': '🔧',
    'contract-renewed': '📋',
    Received: '📦',
    TriageCompleted: '🔍',
    RefurbStarted: '🛠',
    RefurbCompleted: '✅',
    QAPassed: '🏆',
    Sold: '🤝',
    Installed: '🏥',
    ServiceJobOpened: '⚠️',
    ServiceJobClosed: '✅',
    Decommissioned: '🔚',
  };

  const FACT_LABEL: Record<string, string> = {
    'opportunity-opened': 'Opportunity opened',
    'service-request': 'Service request',
    'service-ticket': 'Service request',
    'contract-renewed': 'Contract renewed',
    Received: 'System received',
    TriageCompleted: 'Triage completed',
    RefurbStarted: 'Refurb started',
    RefurbCompleted: 'Refurb completed',
    QAPassed: 'QA passed',
    Sold: 'System sold',
    Installed: 'System installed',
    ServiceJobOpened: 'SR opened',
    ServiceJobClosed: 'SR closed',
    Decommissioned: 'Decommissioned',
  };

  function factKindOf(f: Fact): string {
    return f.fact_kind ?? f.kind ?? 'unknown';
  }
  function factDateOf(f: Fact): string {
    return f.occurred_at ?? f.ts ?? '';
  }
  function factSummaryOf(f: Fact): string {
    const p = f.payload ?? {};
    return (
      (p.summary as string | undefined) ??
      (p.opportunity_id as string | undefined) ??
      (p.ticket_id as string | undefined) ??
      (p.agreement_id as string | undefined) ??
      ''
    );
  }
  function workflowLabelOf(kind: string): string {
    return workflowLabels.get(kind) ?? kind;
  }
</script>

<div class="kb-view">
  <Section title={`Timeline (${facts.length})`} wide>
      {#if factsFailed}
        <div class="kb-empty load-failed" role="alert">
          Couldn't load the timeline — {factsFailed}
        </div>
      {:else if facts.length === 0}
        <div class="kb-empty">No recorded activity yet.</div>
      {:else}
        {#if namesRead.kind === 'failed'}
          <div class="kb-empty load-failed" role="alert">
            Couldn't load the names on the timeline — {namesRead.error}. People show as ids.
          </div>
        {/if}
        <div class="kb-timeline">
          {#each facts.slice(0, 50) as fact, i (fact.id ?? `${factKindOf(fact)}-${i}`)}
            {@const kind = factKindOf(fact)}
            {@const date = factDateOf(fact)}
            {@const summary = factSummaryOf(fact)}
            <div class="kb-timeline-item">
              <span class="kb-timeline-icon">{FACT_ICON[kind] ?? '📌'}</span>
              <div class="kb-timeline-content">
                <span class="kb-timeline-label">{FACT_LABEL[kind] ?? kind}</span>
                {#if summary}
                  <span class="kb-timeline-summary">{summary}</span>
                {/if}
                {#if fact.actor_id}
                  <span class="kb-timeline-actor">
                    {formatActor(fact.actor_id, empNames)}
                  </span>
                {/if}
              </div>
              <span class="kb-timeline-date">{date}</span>
              {#if fact.job_id}
                <Link to={entityHref('job', fact.job_id)} className="kb-timeline-job-link">
                  Job →
                </Link>
              {/if}
            </div>
          {/each}
          {#if facts.length > 50}
            <div class="kb-more">{facts.length - 50} more events not shown</div>
          {/if}
        </div>
      {/if}
  </Section>

  {#if jobsFailed}
    <Section title="Related jobs" wide>
        <div class="kb-empty load-failed" role="alert">
          Couldn't load related jobs — {jobsFailed}
        </div>
    </Section>
  {/if}
  {#if docsFailed}
    <Section title="Documents">
        <div class="kb-empty load-failed" role="alert">
          Couldn't load documents — {docsFailed}
        </div>
    </Section>
  {/if}

  {#if openJobs.length > 0}
    <Section title={`Active jobs (${openJobs.length})`} wide>
        <div class="kb-jobs">
          {#each openJobs as job (job.id)}
            <Link to={entityHref('job', job.id)} className="kb-job-row">
                <span class="kb-workflow">{workflowLabelOf(job.kind)}</span>
                <span class="kb-job-title">{job.title}</span>
                <span class="kb-job-status kb-status-{job.status}">{job.status}</span>
                <span class="kb-job-date">{job.opened_on}</span>
            </Link>
          {/each}
        </div>
    </Section>
  {/if}

  {#if closedJobs.length > 0}
    <Section title={`Completed jobs (${closedJobs.length})`}>
        <div class="kb-jobs">
          {#each closedJobs.slice(0, 10) as job (job.id)}
            <Link to={entityHref('job', job.id)} className="kb-job-row">
                <span class="kb-workflow">{workflowLabelOf(job.kind)}</span>
                <span class="kb-job-title">{job.title}</span>
                <span class="kb-job-status kb-status-{job.status}">{job.status}</span>
                <span class="kb-job-date">{job.opened_on}</span>
            </Link>
          {/each}
        </div>
    </Section>
  {/if}

  {#if docs.length > 0}
    <Section title={`Documents (${docs.length})`}>
        <div class="kb-docs">
          {#each docs as doc (doc.id)}
            <div class="kb-doc-row">
              <span class="kb-doc-type">{doc.doc_type}</span>
              <span class="kb-doc-title">
                {#if doc.url}
                  <a href={doc.url} target="_blank" rel="noopener noreferrer">
                    {doc.title}
                  </a>
                {:else}
                  {doc.title}
                {/if}
              </span>
              {#if doc.version}
                <span class="kb-doc-version">v{doc.version}</span>
              {/if}
              <span class="kb-doc-audience">{doc.audience}</span>
            </div>
          {/each}
        </div>
    </Section>
  {/if}

  <Section title="Insights">
      <div class="kb-insights-placeholder">
        Aggregated knowledge will appear here as more operational data
        accumulates.
        {#if facts.length > 0}
          <div class="kb-insight-stat">
            {facts.length} recorded interactions · {openJobs.length} active
            jobs · {closedJobs.length} completed jobs
          </div>
        {/if}
      </div>
  </Section>
</div>
