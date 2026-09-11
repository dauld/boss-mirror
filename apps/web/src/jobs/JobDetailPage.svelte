<script lang="ts">
  // Job Detail — the work surface for one Job.
  //
  // Hero + subject panel + per-step work surface via StepSurface
  // (which dispatches to the approval surface, the generic surface,
  // or a React plugin bundle based on step.kind).

  import { navigate, href } from '../router';
  import { shortId } from '../data/ids';
  import {
    subjectLabel,
    subjectPath,
    type Job,
  } from './types';
  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import Section from '@boss/web-kit/ui/Section.svelte';
  import StepSurface from '../steps/StepSurface.svelte';
  import StepGraph from './StepGraph.svelte';
  import FileAttachments from '../content/FileAttachments.svelte';
  // Renders only when a step carries an `arrival_report` — a train's
  // landing report. Every other Job renders exactly as before.
  import ArrivalReport from '../it/yard/ArrivalReport.svelte';
  import { fetchRemote, type Remote } from '../data/remote';

  let { jobId } = $props<{ jobId: string }>();

  let job = $state<Job | null>(null);
  let loading = $state(true);
  let error = $state<string | null>(null);

  // The declared job-to-job link fields (job_edges registry) — the
  // registry's first consumer. Read once; resolve this Job's
  // metadata against the declarations. Outbound only in v1 (inbound
  // needs a server-side query; it arrives with the department
  // network view).
  type JobEdgeSpec = Readonly<{
    source_kind: string;
    field_path: string;
    field_kind: string;
    description: string;
  }>;
  // A failed registry read used to leave `edgeSpecs` empty, which reads
  // as "this Job links to nothing" — the false-empty class (backlog
  // a704c5eb; the union is packet 3fba9c35's). Lower stakes than the HR
  // tasks table that packet was filed about — missing link labels, not a
  // claim about a person — but the same silent shape, so it gets the
  // same treatment: a Remote, branched on at the render site.
  let edges = $state<Exclude<Remote<ReadonlyArray<JobEdgeSpec>>, { kind: 'loading' }> | null>(
    null,
  );
  $effect(() => {
    void (async () => {
      edges = await fetchRemote('/api/jobs/job-edges', (raw) => {
        if (!Array.isArray(raw)) throw new Error('job-edges: expected an array');
        return raw as ReadonlyArray<JobEdgeSpec>;
      });
    })();
  });

  let jobLinks = $derived.by(() => {
    const j = job;
    const edgeSpecs = edges?.kind === 'ready' ? edges.data : [];
    if (!j) return [];
    const out: { label: string; description: string; ids: string[] }[] = [];
    for (const e of edgeSpecs) {
      if (e.source_kind !== j.kind) continue;
      const raw = (j.metadata as Record<string, unknown> | undefined)?.[e.field_path];
      const ids =
        e.field_kind === 'job_id_list'
          ? Array.isArray(raw)
            ? raw.filter((v): v is string => typeof v === 'string')
            : []
          : typeof raw === 'string' && raw !== ''
            ? [raw]
            : [];
      if (ids.length > 0) out.push({ label: e.field_path, description: e.description, ids });
    }
    return out;
  });

  // Two paths:
  // 1. /api/jobs/{id}/stream — SSE that pushes a JobDetail frame
  //    on every observable change (job status / priority / closed_on,
  //    or any step's status / completed_on). Per the SSE policy doc
  //    this view is "state-machine state where a single event flips
  //    the visible value" → SSE-push. Shipped 2026-05-01.
  // 2. fetch /api/jobs/{id} — fallback when SSE fails (older deploys).
  //    Also used by post-action callbacks (StepSurface PUT → onStepUpdate)
  //    to refresh immediately rather than waiting for the next SSE
  //    poll tick on the server.
  //
  // The fallback fetches RACE: the 30s poll, a post-action refetch,
  // and a fast A→B navigation can all have answers in flight at once,
  // and without a guard the slowest one wins — packet A rendering
  // under B's URL was the audit's worst case (2026-08-21). So every
  // question carries a ticket: a monotonic sequence plus the jobId it
  // was asked about, checked before ANY assignment. Plain variables,
  // not $state — they gate writes, they never render.
  let loadSeq = 0;
  // False until the first answer (fetch or SSE frame) for the CURRENT
  // jobId lands. Only that first wait blanks the page; a later
  // refresh keeps the content it has — a 30s poll that blanked the
  // whole page every cycle was the audit's other finding.
  let firstAnswered = false;
  let refreshing = $state(false);

  async function load() {
    const id = jobId;
    const seq = ++loadSeq;
    if (firstAnswered) {
      refreshing = true;
    } else {
      loading = true;
    }
    try {
      const resp = await fetch(`/api/jobs/${encodeURIComponent(id)}`);
      if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
      const detail = (await resp.json()) as Job;
      // The ticket check: a newer question has been asked, or the
      // page has moved to a different packet. Either way this answer
      // is history — drop it entirely.
      if (seq !== loadSeq || id !== jobId) return;
      job = detail;
      error = null;
    } catch (e) {
      if (seq !== loadSeq || id !== jobId) return;
      error = e instanceof Error ? e.message : String(e);
    } finally {
      if (seq === loadSeq && id === jobId) {
        firstAnswered = true;
        loading = false;
        refreshing = false;
      }
    }
  }

  $effect(() => {
    const id = jobId;
    let es: EventSource | null = null;
    let pollFallbackId: number | null = null;
    let cancelled = false;

    // A different packet starts from scratch: A's content must not
    // render under B's URL while B loads, and any answer still in
    // flight about A is stranded by the seq bump.
    loadSeq++;
    firstAnswered = false;
    job = null;
    error = null;
    loading = true;
    refreshing = false;

    try {
      es = new EventSource(`/api/jobs/${encodeURIComponent(id)}/stream`);
      es.onmessage = (ev) => {
        if (cancelled) return;
        try {
          const detail = JSON.parse(ev.data) as Job;
          job = detail;
          firstAnswered = true;
          loading = false;
          refreshing = false;
          error = null;
        } catch {
          // Drop malformed frame; next push will fix it.
        }
      };
      es.addEventListener('error', () => {
        if (es && es.readyState === EventSource.CLOSED) {
          es.close();
          es = null;
          // On 404 / proxy down, fall back to the on-mount fetch
          // + slow-poll. On a transient blip the browser
          // auto-reconnects, so we only fall through on CLOSED.
          if (pollFallbackId === null) {
            void load();
            pollFallbackId = window.setInterval(() => void load(), 30_000);
          }
        }
      });
      es.addEventListener('gone', () => {
        // Server says the Job is gone — refetch once so error
        // state lands consistently.
        void load();
      });
    } catch {
      void load();
      pollFallbackId = window.setInterval(() => void load(), 30_000);
    }

    return () => {
      cancelled = true;
      es?.close();
      if (pollFallbackId !== null) window.clearInterval(pollFallbackId);
    };
  });

  function onStepUpdate(): void {
    // Eager refetch after the operator clicks done/sign-off so
    // the page updates without waiting for the next 2s SSE tick.
    void load();
  }
</script>

{#if loading && !job}
  <!-- Only the FIRST wait for a packet blanks the page. Refreshes
       (the 30s fallback poll, post-action refetches) keep the content
       they have and mark themselves quietly below. -->
  <div class="catalog theme-exec"><p class="empty">Loading…</p></div>
{:else if !job}
  <!-- No content to keep — the first load failed, so the error is the
       page. Once content exists, a failed REFRESH keeps it instead
       (the next poll tick or SSE frame corrects); swapping a rendered
       packet for an error on one blip is the modal's poisoning bug at
       page scale. -->
  <div class="catalog theme-exec">
    <p class="empty">Couldn't load job: {error ?? 'not found'}</p>
  </div>
{:else}
  {@const j = job}
  <div class="catalog theme-exec">
    <PageHeader
      eyebrow={`${j.kind} · ${j.status}${refreshing ? ' · refreshing…' : ''}`}
      title={j.title}
      subtitle={`Opened ${j.opened_on}${j.due_on ? ` · due ${j.due_on}` : ''} · owner ${j.owner_id}`}
    />

    <div class="tab-grid">
      <ArrivalReport job={j} />
      {#if edges?.kind === 'failed'}
        <!-- Saying nothing here would be saying "no linked Jobs", which
             the failed read has no standing to claim. -->
        <Section title="Linked Jobs">
          <p class="load-failed" role="alert" style="font-size:13px">
            Couldn't read the job-edges registry — {edges.error}. Any links
            this Job declares are unknown, not absent.
          </p>
        </Section>
      {:else if jobLinks.length > 0}
        <Section title="Linked Jobs">
          {#each jobLinks as link (link.label)}
            <div class="jd-info-row">
              <span class="jd-info-label" title={link.description}>{link.label}</span>
              <span class="jd-info-value jd-mono">
                {#each link.ids as lid, i (lid)}
                  {#if i > 0}<span>, </span>{/if}
                  <a
                    href={href(`/jobs/${lid}`)}
                    onclick={(e) => {
                      e.preventDefault();
                      navigate(href(`/jobs/${lid}`));
                    }}
                  >{lid.slice(0, 8)}</a>
                {/each}
              </span>
            </div>
          {/each}
        </Section>
      {/if}
      <Section title="Subject">
          <div class="jd-info-row">
            <span class="jd-info-label">Kind</span>
            <span class="jd-info-value">{j.subject.subject_kind}</span>
          </div>
          <div class="jd-info-row">
            <span class="jd-info-label">ID</span>
            <span class="jd-info-value jd-mono">
              <a
                href={href(subjectPath(j.subject))}
                onclick={(e) => {
                  e.preventDefault();
                  navigate(href(subjectPath(j.subject)));
                }}
              >
                {subjectLabel(j.subject)}
              </a>
            </span>
          </div>
          <div class="jd-info-row">
            <span class="jd-info-label">BOSS Job ID</span>
            <span class="jd-info-value jd-mono">{shortId(j.id)}</span>
          </div>
      </Section>

      <Section title="Attachments">
        <FileAttachments targetKind="job" targetId={j.id} />
      </Section>

      <Section title={`Steps (${j.steps?.length ?? 0})`} wide>
          {#if !j.steps || j.steps.length === 0}
            <p class="empty">No steps on this job yet.</p>
          {:else}
            <StepGraph
              steps={j.steps.map((s) => ({ ...s, notes: s.notes ?? null }))}
              {jobId}
              onUpdate={onStepUpdate}
            />
          {/if}
      </Section>
    </div>
  </div>
{/if}
