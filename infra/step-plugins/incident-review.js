// incident-review.js — custom Step UX for the incident-post-mortem
// Workflow's "Human review of the findings" step.
//
// WHY THIS EXISTS. Two feedback packets said the review step renders
// the findings unusably; one asked for "a custom step UX that
// presented the findings that I needed to sign-off on". The failure is
// structural, the same shape correction-verdict.js fixed: everything
// the reviewer needs to judge lives elsewhere — the Job's own metadata
// (summary, evidence, mitigations, open questions) and the answers the
// earlier steps recorded (timeline, attribution, detection,
// simplification, actions) — all of it behind a packet modal and a
// metadata dump. The question was on screen; the material to answer it
// was not.
//
// So this surface renders the whole post-mortem as ONE readable
// document — the Job's semi-structured metadata as ordered sections,
// then what each step found, labeled by its fields — and closes with
// the completion the step requires. Nothing here is new information;
// it is the same data, arranged to be read.
//
// THE RENDERER IS SEMI-STRUCTURED, deliberately. The two live packets
// already carry different metadata shapes. Keys the platform knows get
// first-class sections in reading order; every other key renders as a
// labeled prose block. Content is never dropped and never dumped as
// raw JSON. `sectionsFor` below is a deliberate near-copy of
// apps/web/src/it/incidents/postMortemDoc.ts — plugins are standalone
// JS bundles by design (no imports from the SPA), so the ordering is
// duplicated. Change one, change both.
//
// WHY A NEW KIND AND NOT A PLUGIN ON `task`. Plugins register by step
// kind and the SPA mounts by kind; registering for `task` would hijack
// every task step in the system (same rationale recorded on
// 146-correction-verdict-plugin.sql). The Workflow's review step moves
// to kind=incident-review in the next workflow version; the Rust
// StepRegistry does not need to learn the kind (validate_metadata is
// permissive for kinds it does not know).
//
// Plugin contract: window.__boss_register_step_plugin(kind, mount).
// Host calls mount(container, props) with { step, jobId, onUpdate }.

(function () {
  // ---------------------------------------------------------------
  // Self-contained styling, injected once, scoped under
  // `.step-incident-review`, written against core's CSS custom
  // properties so it follows the tenant's theme rather than fighting
  // it. Same reasoning as review-design.js / correction-verdict.js.
  // ---------------------------------------------------------------
  const STYLE_ID = 'boss-incident-review-styles';
  const STYLES = `
.step-incident-review { max-width: 78ch; }

.step-incident-review .sir-head {
  display: flex; align-items: baseline; gap: 12px; flex-wrap: wrap;
  padding-bottom: 10px; margin-bottom: 18px;
  border-bottom: 1px solid var(--border, var(--hairline));
}
.step-incident-review .sir-head h3 { margin: 0; font-size: 17px; flex: 1 1 auto; }
.step-incident-review .sir-when {
  font-size: 12px; color: var(--text-dim, var(--static)); white-space: nowrap;
}

.step-incident-review .sir-section { margin: 0 0 14px; }
.step-incident-review .sir-label {
  margin: 0 0 3px; font-size: 11px; font-weight: 600;
  letter-spacing: 0.06em; text-transform: uppercase;
  color: var(--text-dim, var(--static));
}
.step-incident-review .sir-body {
  margin: 0; font-size: 13.5px; line-height: 1.6;
  white-space: pre-wrap; word-break: break-word;
  color: var(--text, var(--fog));
}

.step-incident-review .sir-steps-title {
  margin: 22px 0 10px; padding-top: 12px; font-size: 14px;
  border-top: 1px solid var(--border, var(--hairline));
}
.step-incident-review .sir-step {
  border: 1px solid var(--border, var(--hairline));
  border-left: 3px solid var(--accent, var(--signal));
  padding: 10px 14px; margin: 0 0 10px;
}
.step-incident-review .sir-step.sir-skipped {
  border-left-color: var(--border, var(--hairline));
}
.step-incident-review .sir-step h4 {
  margin: 0 0 8px; font-size: 13px;
}
.step-incident-review .sir-step .sir-by {
  font-weight: 400; font-size: 11px; color: var(--text-dim, var(--static));
}

.step-incident-review .sir-empty {
  font-size: 13px; font-style: italic; color: var(--text-dim, var(--static));
}
.step-incident-review .sir-err {
  font-size: 12px; color: var(--err, currentColor);
}

.step-incident-review .sir-actions {
  display: flex; align-items: center; gap: 12px;
  margin-top: 18px; padding-top: 12px;
  border-top: 1px solid var(--border, var(--hairline));
}
.step-incident-review .sir-actions button {
  font: inherit; font-size: 13px; font-weight: 600;
  padding: 7px 18px; cursor: pointer;
  border: 1px solid var(--accent, var(--signal));
  background: transparent; color: var(--accent, var(--signal));
}
.step-incident-review .sir-actions button:hover:not([disabled]) {
  background: var(--accent, var(--signal));
  color: var(--bg, var(--void));
}
.step-incident-review .sir-actions button[disabled] { opacity: 0.5; cursor: default; }
.step-incident-review .sir-done {
  font-size: 13px; padding: 10px 14px; margin-top: 18px;
  border: 1px solid var(--border, var(--hairline));
  color: var(--text-dim, var(--static));
}
`;

  function injectStyles() {
    if (document.getElementById(STYLE_ID)) return;
    const el = document.createElement('style');
    el.id = STYLE_ID;
    el.textContent = STYLES;
    document.head.appendChild(el);
  }

  function esc(s) {
    return String(s == null ? '' : s).replace(/[&<>"']/g, (c) => ({
      '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;',
    })[c]);
  }

  function humanize(key) {
    const spaced = String(key).replace(/[_-]+/g, ' ').trim();
    return spaced.charAt(0).toUpperCase() + spaced.slice(1);
  }

  // Flatten a metadata value to prose. Mirrors postMortemDoc.ts:
  // strings pass through, scalars stringify, arrays become lines, flat
  // objects become labeled lines. null = nothing to render.
  function prose(value) {
    if (value === null || value === undefined) return null;
    if (typeof value === 'string') return value.trim() === '' ? null : value;
    if (typeof value === 'number' || typeof value === 'boolean') return String(value);
    if (Array.isArray(value)) {
      if (value.length === 0) return null;
      return value.map((v) => (typeof v === 'string' ? v : JSON.stringify(v))).join('\n');
    }
    if (typeof value === 'object') {
      const entries = Object.entries(value);
      if (entries.length === 0) return null;
      return entries
        .map(([k, v]) => humanize(k) + ': ' + (typeof v === 'string' ? v : JSON.stringify(v)))
        .join('\n');
    }
    return String(value);
  }

  // Reading order + labels for the keys the platform knows — the
  // deliberate near-copy of postMortemDoc.ts (see the header comment).
  const KNOWN_LABELS = {
    incident_at: 'When it happened',
    incident_date: 'When it happened',
    summary: 'Summary',
    timeline: 'Timeline',
    root_cause: 'Root cause',
    open_questions: 'Open questions',
    evidence: 'Evidence',
  };
  const READING_ORDER = [
    'incident_at', 'incident_date', 'summary', 'timeline', 'root_cause',
    '#mitigations', 'open_questions', 'evidence',
  ];

  function sectionsFor(metadata, omit) {
    const skip = new Set(omit || []);
    const mitigationsSlot = READING_ORDER.indexOf('#mitigations');
    const rank = (key) => {
      const slot = READING_ORDER.indexOf(key);
      if (slot >= 0) return slot;
      if (key.indexOf('mitigation') === 0) return mitigationsSlot;
      return READING_ORDER.length;
    };
    return Object.keys(metadata || {})
      .filter((key) => !skip.has(key))
      .map((key, authored) => ({ key, authored }))
      .sort((a, b) => rank(a.key) - rank(b.key) || a.authored - b.authored)
      .map(({ key }) => ({
        key,
        label: KNOWN_LABELS[key] || humanize(key),
        body: prose(metadata[key]),
      }))
      .filter((s) => s.body !== null);
  }

  // Plumbing keys that are not findings: authority gating and trigger
  // bookkeeping, plus agent hand-off records.
  const PLUMBING = new Set([
    'authority_role', 'trigger_kind', 'trigger_name',
    'agent_requested_at', 'agent_requested_by',
  ]);

  function sectionHtml(s) {
    return (
      '<section class="sir-section">' +
      '<h5 class="sir-label">' + esc(s.label) + '</h5>' +
      '<p class="sir-body">' + esc(s.body) + '</p>' +
      '</section>'
    );
  }

  function mount(container, props) {
    injectStyles();
    const step = props.step;
    const jobId = props.jobId;
    const onUpdate = props.onUpdate;

    let job = null;
    let loadError = null;
    let saving = false;
    let saveError = null;

    const root = document.createElement('div');
    root.className = 'step-surface step-incident-review';
    container.appendChild(root);

    const terminal = step.status === 'completed' || step.status === 'skipped';

    function findingsHtml() {
      if (loadError) {
        // The findings are the point — say so rather than rendering a
        // confident-looking empty review (false-empty sweep).
        return '<p class="sir-err">Could not load the post-mortem findings — ' +
          esc(loadError) + '. Reload before reviewing.</p>';
      }
      if (!job) return '<p class="sir-empty">Loading the findings…</p>';

      const meta = job.metadata || {};
      const when = prose(meta.incident_at) || prose(meta.incident_date);
      const sections = sectionsFor(meta, ['incident_at', 'incident_date']);

      // What each sibling step found: terminal steps other than this
      // one, in workflow order, each field labeled. A step whose
      // metadata is all plumbing recorded no findings and is omitted.
      const siblings = (Array.isArray(job.steps) ? job.steps : [])
        .filter((s) => s.id !== step.id)
        .filter((s) => s.status === 'completed' || s.status === 'skipped')
        .sort((a, b) => (a.sort_order || 0) - (b.sort_order || 0))
        .map((s) => ({
          step: s,
          found: sectionsFor(s.metadata || {}, [...PLUMBING]),
        }))
        .filter((x) => x.found.length > 0);

      return (
        '<div class="sir-head">' +
        '<h3>' + esc(job.title || 'Post-mortem findings') + '</h3>' +
        (when ? '<span class="sir-when">' + esc(when) + '</span>' : '') +
        '</div>' +
        (sections.length
          ? sections.map(sectionHtml).join('')
          : '<p class="sir-empty">The packet carries no findings in its metadata yet.</p>') +
        (siblings.length
          ? '<h4 class="sir-steps-title">What each step found</h4>' +
            siblings
              .map(
                (x) =>
                  '<article class="sir-step' +
                  (x.step.status === 'skipped' ? ' sir-skipped' : '') +
                  '">' +
                  '<h4>' + esc(x.step.title) +
                  (x.step.assignee_id
                    ? ' <span class="sir-by">· ' + esc(x.step.assignee_id) + '</span>'
                    : '') +
                  '</h4>' +
                  x.found.map(sectionHtml).join('') +
                  '</article>',
              )
              .join('')
          : '')
      );
    }

    function actionsHtml() {
      if (terminal) {
        return '<div class="sir-done">Review recorded — this step is ' +
          esc(step.status) + '. The document above is the durable record.</div>';
      }
      // A findings load failure must not offer completion: reviewing a
      // post-mortem nobody has seen is not a review (same gate
      // review-design.js carries).
      if (loadError || !job) return '';
      return (
        '<div class="sir-actions">' +
        '<button type="button" data-action="complete"' + (saving ? ' disabled' : '') + '>' +
        (saving ? 'Recording…' : 'Findings reviewed — complete review') +
        '</button>' +
        (saveError ? '<span class="sir-err">' + esc(saveError) + '</span>' : '') +
        '</div>'
      );
    }

    function render() {
      root.innerHTML = findingsHtml() + actionsHtml();
      const btn = root.querySelector('[data-action=complete]');
      if (btn) btn.addEventListener('click', complete);
    }

    async function complete() {
      if (saving) return;
      saving = true;
      saveError = null;
      render();
      try {
        // This surface records no metadata keys of its own, so there
        // is nothing to merge through the step metadata PATCH — but
        // the completion PUT still replaces metadata wholesale, and it
        // used to re-send the page-load snapshot: a lost update that
        // silently reverted any key recorded server-side after this
        // page loaded. Read the row as it stands and attest THAT.
        // (No single-step GET exists; the job's steps list is the
        // read the API offers.)
        const lr = await fetch(`/api/jobs/${jobId}/steps`);
        if (!lr.ok) {
          throw new Error(`step read-back HTTP ${lr.status}: ${await lr.text()}`);
        }
        const stepsNow = await lr.json();
        const fresh = Array.isArray(stepsNow)
          ? stepsNow.find((s) => s.id === step.id)
          : null;
        if (!fresh) throw new Error('step read-back: step missing from its own job');
        // Stamp every required sign-off role first, in the step's
        // final shape (v1 of the workflow requires none; this stays
        // generic so a v2 that adds one keeps working).
        for (const role of step.sign_offs_required || []) {
          const sr = await fetch(`/api/jobs/${jobId}/steps/${step.id}/sign-offs`, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ role }),
          });
          if (!sr.ok) {
            throw new Error(`sign-off as ${role} failed (HTTP ${sr.status}): ${await sr.text()}`);
          }
        }
        const r = await fetch(`/api/jobs/${jobId}/steps/${step.id}`, {
          method: 'PUT',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify(
            Object.assign({}, fresh, { job_id: jobId, status: 'completed' }),
          ),
        });
        // Read the code — a swallowed non-2xx leaves the surface
        // looking saved while the packet never moved.
        if (!r.ok) throw new Error(`HTTP ${r.status}: ${await r.text()}`);
        onUpdate();
      } catch (e) {
        saveError = e && e.message ? e.message : String(e);
        saving = false;
        render();
      }
    }

    // First paint immediately so the surface is never blank; the
    // document fills in when the Job lands. One fetch: the list
    // endpoint already enriches the Job with its steps.
    render();
    fetch(`/api/jobs/${jobId}`)
      .then((r) => {
        if (!r.ok) throw new Error(`HTTP ${r.status}`);
        return r.json();
      })
      .then((j) => {
        job = j;
        render();
      })
      .catch((e) => {
        loadError = e && e.message ? e.message : String(e);
        render();
      });

    return function cleanup() {
      root.remove();
    };
  }

  if (typeof window.__boss_register_step_plugin !== 'function') {
    console.error('[incident-review-plugin] __boss_register_step_plugin not on window');
    return;
  }
  window.__boss_register_step_plugin('incident-review', mount);
})();
