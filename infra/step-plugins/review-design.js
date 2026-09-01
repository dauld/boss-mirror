// review-design.js — custom Step UX for the design-doc-review JobKind.
//
// Reads step.metadata.doc_path, fetches /api/design/docs/{path} to
// get the design doc + its parsed open questions (### Qn: <title>
// headings under ## Open Questions). Renders a per-question
// resolution textarea. Step completion is GATED on every question
// having a non-empty resolution recorded.
//
// Resolutions are saved as pending-decisions via
// /api/design/pending-decisions; the follow-up
// /api/design/flush-jobs endpoint writes them into the source
// doc's Decision-history section (each release, settled material
// folds into docs/architecture-decisions.md and the source doc is
// deleted). Brings back the "system models its own development"
// workflow that existed pre-2026-05-03.
//
// Plugin contract: window.__boss_register_step_plugin(kind, mount).
// Host calls mount(container, props) with { step, jobId, onUpdate }.

(function () {
  // ---------------------------------------------------------------
  // Self-contained styling.
  //
  // The markup below used semantic `.step-review-*` class names that
  // nothing styled: core's stylesheet only carries the generic
  // `.step-surface` wrapper, and a plugin has no business adding rules
  // to core anyway — that is the whole point of shipping UX as a
  // bundle. So the surface rendered at browser defaults: full-width
  // unmeasured prose, bare textareas, no hierarchy. Readable in the
  // sense that the characters were present.
  //
  // Injected once, scoped under `.step-review-design`, and written
  // against core's CSS custom properties (with fallbacks) so it
  // inherits the tenant's light/dark theme instead of fighting it.
  // ---------------------------------------------------------------
  const STYLE_ID = 'boss-review-design-styles';
  const STYLES = `
.step-review-design { --srd-gap: 20px; }

/* Header: title, status, and the progress meter. */
.step-review-design .srd-head {
  display: flex; align-items: baseline; gap: 12px; flex-wrap: wrap;
  padding-bottom: 12px; margin-bottom: var(--srd-gap);
  border-bottom: 1px solid var(--border, #e7e5e4);
}
.step-review-design .srd-head h3 { margin: 0; font-size: 17px; flex: 1 1 auto; }
.step-review-design .srd-fullpage {
  font-size: 12px; color: var(--accent, #2563eb); text-decoration: none;
  white-space: nowrap; align-self: center;
}
.step-review-design .srd-fullpage:hover { text-decoration: underline; }
.step-review-design .srd-progress {
  display: flex; align-items: center; gap: 8px;
  font-size: 12px; color: var(--text-dim, #78716c); white-space: nowrap;
}
.step-review-design .srd-meter {
  width: 120px; height: 6px; border-radius: 3px;
  background: var(--border, #e7e5e4); overflow: hidden;
}
.step-review-design .srd-meter > i {
  display: block; height: 100%; width: 0%;
  background: var(--accent, #2563eb); transition: width .2s ease;
}
.step-review-design .srd-meter.is-complete > i { background: #16a34a; }

/* Two panes: the document to read, the decisions to record.

   The rail may grow to half (David, 6d4fa80a: "give the question bar
   up to half if it doesn't require scrolling the doc panel
   horizontally"). It was capped at 420px while the doc took every
   remaining pixel — but the doc CANNOT use them: .srd-doc-inner is
   capped at 68ch, so past that width the left pane was growing its
   own margins while the rail, which holds the textareas you actually
   type in, stayed narrow.

   Both columns are 1fr, so on a wide viewport they split evenly. The
   left floor is what honours "without scrolling the doc panel": 46ch
   is narrower than the 68ch measure but still comfortably wider than
   the point at which prose starts to break up, and the elements that
   genuinely cannot reflow — pre blocks and tables — already scroll
   inside their own box rather than widening the panel. */
.step-review-design .srd-panes {
  display: grid; grid-template-columns: minmax(46ch, 1fr) minmax(320px, 1fr);
  gap: var(--srd-gap); align-items: start;
}
@media (max-width: 1100px) {
  .step-review-design .srd-panes { grid-template-columns: minmax(0, 1fr); }
  .step-review-design .srd-rail { position: static !important; max-height: none !important; }
}

/* Document pane — the reading surface. A measure, a line-height, and
   room to breathe; this is the half that was unreadable. */
.step-review-design .srd-doc {
  background: var(--card, #fff); border: 1px solid var(--border, #e7e5e4);
  border-radius: 8px; padding: 28px 32px;
  max-height: 78vh; overflow-y: auto;
}
.step-review-design .srd-doc-inner { max-width: 68ch; }
.step-review-design .srd-doc-inner > * { max-width: 100%; }
.step-review-design .srd-doc-inner p,
.step-review-design .srd-doc-inner li {
  font-size: 15px; line-height: 1.7; color: var(--text, #1c1917);
}
.step-review-design .srd-doc-inner h1 { font-size: 22px; margin: 0 0 4px; line-height: 1.3; }
.step-review-design .srd-doc-inner h2 {
  font-size: 17px; margin: 32px 0 10px; padding-top: 14px;
  border-top: 1px solid var(--border, #e7e5e4); line-height: 1.35;
}
.step-review-design .srd-doc-inner h3 { font-size: 15px; margin: 22px 0 6px; line-height: 1.4; }
.step-review-design .srd-doc-inner code {
  font-size: 0.9em; padding: 1px 4px; border-radius: 3px;
  background: var(--bg, #f5f5f4);
}
.step-review-design .srd-doc-inner pre {
  background: var(--bg, #f5f5f4); padding: 12px 14px; border-radius: 6px;
  overflow-x: auto; font-size: 13px; line-height: 1.55;
}
.step-review-design .srd-doc-inner blockquote {
  margin: 16px 0; padding: 2px 0 2px 16px;
  border-left: 3px solid var(--accent, #2563eb); color: var(--text-dim, #78716c);
}
.step-review-design .srd-doc-inner table { border-collapse: collapse; font-size: 14px; }
.step-review-design .srd-doc-inner th,
.step-review-design .srd-doc-inner td {
  border: 1px solid var(--border, #e7e5e4); padding: 6px 10px; text-align: left;
}
.step-review-design .srd-rawmd {
      white-space: pre-wrap;
      word-break: break-word;
      font: 12px/1.55 ui-monospace, SFMono-Regular, Menlo, monospace;
      margin: 12px 0 0;
    }
    .srd-docmeta {
  font-size: 12px; color: var(--text-dim, #78716c);
  margin-bottom: 18px; padding-bottom: 10px;
  border-bottom: 1px solid var(--border, #e7e5e4);
}

/* Decision rail — sticky so the questions stay put while you read. */
.step-review-design .srd-rail {
  position: sticky; top: 12px;
  max-height: 78vh; overflow-y: auto;
  display: flex; flex-direction: column; gap: 12px;
}
.step-review-design .srd-rail-title {
  font-size: 12px; font-weight: 600; letter-spacing: .04em;
  text-transform: uppercase; color: var(--text-dim, #78716c);
}
.step-review-design .srd-q {
  border: 1px solid var(--border, #e7e5e4); border-left: 3px solid var(--border, #e7e5e4);
  border-radius: 6px; padding: 14px 16px; background: var(--card, #fff);
}
.step-review-design .srd-q.is-addressed { border-left-color: #16a34a; }
.step-review-design .srd-q-head { display: flex; gap: 8px; align-items: baseline; }
.step-review-design .srd-anchor {
  font-size: 11px; font-weight: 700; padding: 1px 6px; border-radius: 3px;
  background: var(--bg, #f5f5f4); color: var(--text-dim, #78716c); flex: none;
}
.step-review-design .srd-q.is-addressed .srd-anchor { background: #dcfce7; color: #15803d; }
.step-review-design .srd-q-title { font-size: 14px; font-weight: 600; line-height: 1.4; }
.step-review-design .srd-q-body {
  font-size: 13px; line-height: 1.6; color: var(--text-dim, #78716c);
  margin: 8px 0 0; max-height: 8.5em; overflow-y: auto;
}
.step-review-design .srd-q-body p { margin: 0 0 8px; }
.step-review-design .srd-label {
  display: block; font-size: 11px; font-weight: 600; letter-spacing: .04em;
  text-transform: uppercase; color: var(--text-dim, #78716c); margin: 12px 0 4px;
}
.step-review-design .srd-q textarea {
  width: 100%; box-sizing: border-box; resize: vertical;
  font: inherit; font-size: 13px; line-height: 1.55;
  padding: 8px 10px; border-radius: 5px;
  border: 1px solid var(--border, #e7e5e4);
  background: var(--bg, #fafaf9); color: var(--text, #1c1917);
}
.step-review-design .srd-q textarea:focus {
  outline: 2px solid var(--accent, #2563eb); outline-offset: -1px; background: var(--card, #fff);
}
.step-review-design .srd-q textarea:disabled { opacity: .7; }

/* The proposal is an offer, so it is visibly NOT the resolution box:
   its own tinted card, above the label, with the button that copies it
   down. Reading line-height — this is prose the reviewer has to weigh,
   not a UI string. */
.step-review-design .srd-proposal {
  margin: 12px 0 0; padding: 8px 10px; border-radius: 5px;
  border: 1px solid var(--border, #e7e5e4);
  border-left: 3px solid var(--accent, #2563eb);
  background: var(--bg, #fafaf9);
}
.step-review-design .srd-proposal-head {
  display: flex; align-items: center; justify-content: space-between; gap: 8px;
}
.step-review-design .srd-proposal-label {
  font-size: 11px; font-weight: 600; letter-spacing: .04em;
  text-transform: uppercase; color: var(--text-dim, #78716c);
}
.step-review-design .srd-proposal-text {
  margin-top: 6px; font-size: 13px; line-height: 1.55; color: var(--text, #1c1917);
  white-space: pre-wrap;
}
.step-review-design .srd-use {
  flex: none; font: inherit; font-size: 12px; cursor: pointer;
  padding: 3px 10px; border-radius: 4px;
  border: 1px solid var(--accent, #2563eb);
  background: transparent; color: var(--accent, #2563eb);
}
.step-review-design .srd-use:hover:not(:disabled) {
  background: var(--accent, #2563eb); color: #fff;
}
.step-review-design .srd-use:disabled { opacity: .5; cursor: default; }

.step-review-design .srd-empty,
.step-review-design .srd-loading {
  padding: 20px; border-radius: 6px; background: var(--bg, #f5f5f4);
  color: var(--text-dim, #78716c); font-size: 14px;
}
.step-review-design .srd-error {
  padding: 12px 14px; border-radius: 6px; font-size: 13px; line-height: 1.5;
  background: #fef2f2; border: 1px solid #fecaca; color: #b91c1c;
}
.step-review-design .step-actions { margin-top: var(--srd-gap); display: flex; gap: 10px; }
`;

  function injectStyles() {
    if (document.getElementById(STYLE_ID)) return;
    const el = document.createElement('style');
    el.id = STYLE_ID;
    el.textContent = STYLES;
    document.head.appendChild(el);
  }

  function h(tag, attrs, ...children) {
    const el = document.createElement(tag);
    if (attrs) {
      for (const k in attrs) {
        const v = attrs[k];
        if (v == null || v === false) continue;
        if (k === 'className') el.className = v;
        else if (k.startsWith('on') && typeof v === 'function') {
          el.addEventListener(k.slice(2).toLowerCase(), v);
        } else if (k === 'checked' || k === 'disabled' || k === 'value') {
          el[k] = v;
        } else {
          el.setAttribute(k, String(v));
        }
      }
    }
    for (const child of children.flat()) {
      if (child == null || child === false) continue;
      el.appendChild(child instanceof Node ? child : document.createTextNode(String(child)));
    }
    return el;
  }

  function mount(container, { step, jobId, onUpdate }) {
    const docPath = (step.metadata && step.metadata.doc_path) || '';
    // resolutions: [{ anchor, decision }] — anchor matches the
    // question anchor returned by /api/design/docs/{path}
    // (e.g. "Q1", "Q2", ...).
    let resolutions = Array.isArray(step.metadata && step.metadata.resolutions)
      ? step.metadata.resolutions.map((r) => ({
          anchor: String(r.anchor || ''),
          decision: String(r.decision || ''),
        }))
      : [];

    let doc = null;
    let questions = [];
    // True when the questions came from the packet rather than the
    // docs API. Decides whether answers are mirrored to
    // pending-decisions: a self-carried packet's answers live in step
    // metadata, which IS the record, so mirroring them into the flush
    // pipeline would create a second copy that can disagree.
    let selfCarried = false;
    let loadError = null;
    let saving = false;
    let saveError = null;
    const isDone = step.status === 'completed' || step.status === 'done';

    const headerDiv = h('div', { className: 'srd-head' });
    const bodyDiv = h('div', { className: 'srd-panes' });
    const actionsDiv = h('div', { className: 'step-actions' });

    function resolutionFor(anchor) {
      const r = resolutions.find((x) => x.anchor === anchor);
      return r ? r.decision : '';
    }

    function setResolution(anchor, decision) {
      const idx = resolutions.findIndex((x) => x.anchor === anchor);
      if (idx >= 0) {
        resolutions[idx] = { anchor, decision };
      } else {
        resolutions.push({ anchor, decision });
      }
      renderActions();
      renderProgress();
    }

    function answeredCount() {
      return questions.filter((q) => resolutionFor(q.anchor).trim().length > 0).length;
    }
    function allAnswered() {
      return questions.length > 0 && answeredCount() === questions.length;
    }

    /// The doc's own proposed answer, with a button that copies it into
    /// the resolution box. Returns null when the question proposes
    /// nothing, which is most of the corpus' older questions and every
    /// question whose author left the answer open on purpose.
    ///
    /// `q.proposal` is parsed by boss-docs from a `Proposed:` line.
    /// That extractor recognised only `**Proposal**:` until 2026-08-14
    /// — a spelling no doc uses — so this field was null on every
    /// question in the corpus and the rail below carries a comment
    /// concluding there was no proposal to accept.
    function proposalBlock(q, onUse) {
      const proposal = typeof q.proposal === 'string' ? q.proposal.trim() : '';
      if (!proposal) return null;
      const btn = h(
        'button',
        { className: 'srd-use', type: 'button', disabled: isDone },
        'Use this',
      );
      btn.addEventListener('click', () => onUse(proposal));
      return h(
        'div',
        { className: 'srd-proposal' },
        h(
          'div',
          { className: 'srd-proposal-head' },
          h('span', { className: 'srd-proposal-label' }, 'Proposed in the doc'),
          btn,
        ),
        h('div', { className: 'srd-proposal-text' }, proposal),
      );
    }

    const progressSpan = h('span', { className: 'srd-progress' });

    function renderProgress() {
      progressSpan.replaceChildren();
      if (loadError) return;
      if (!questions.length) {
        progressSpan.appendChild(h('span', null, 'no open questions'));
        return;
      }
      const done = answeredCount();
      const meter = h('span', {
        className: `srd-meter ${done === questions.length ? 'is-complete' : ''}`,
      });
      const fill = h('i');
      fill.style.width = `${Math.round((done / questions.length) * 100)}%`;
      meter.appendChild(fill);
      progressSpan.appendChild(meter);
      progressSpan.appendChild(
        h('span', null, `${done}/${questions.length} addressed`),
      );
    }

    function renderHeader() {
      // The reviewer's escape hatch (7501ef82: "give me the option
      // for a full-panel experience if I want. I know we have it") —
      // the full-page step surface exists at /jobs/{job}/steps/{step};
      // the embedded panel just never pointed at it.
      const fullPage = h(
        'a',
        {
          className: 'srd-fullpage',
          href: `/jobs/${jobId}/steps/${step.id}`,
          title: 'Open this review as a full page',
        },
        'Full page \u2197',
      );
      headerDiv.replaceChildren(
        h('h3', null, step.title),
        h('span', { className: `step-status step-status-${step.status}` }, step.status),
        fullPage,
        progressSpan,
      );
    }

    function renderBody() {
      bodyDiv.replaceChildren();
      if (loadError) {
        bodyDiv.appendChild(
          h('div', { className: 'srd-error' }, `Failed to load doc: ${loadError}`),
        );
        return;
      }
      if (!doc) {
        bodyDiv.appendChild(h('div', { className: 'srd-loading' }, 'Loading the doc…'));
        return;
      }

      // ---- Pane 1: the document, as something to actually read. ----
      // Always open. It was behind a collapsed <details> summarised as
      // "Read the doc" — one more click between a reviewer and the
      // thing they are reviewing.
      const docPane = h('div', { className: 'srd-doc' });
      const inner = h('div', { className: 'srd-doc-inner' });
      // A self-carried packet has no path/status/word_count — it is not
      // a file yet, which is the point. Say what it IS instead of
      // rendering three `undefined`s.
      inner.appendChild(
        h(
          'div',
          { className: 'srd-docmeta' },
          doc.path
            ? `${doc.path} · ${doc.status} · ${doc.word_count || '—'} words`
            : 'carried by this packet · not yet a file',
        ),
      );
      if (doc.content_html) {
        const prose = h('div');
        // Server-rendered from the repo-committed markdown by the same
        // pulldown_cmark pipeline that renders the design page — same
        // trust domain as this bundle.
        prose.innerHTML = doc.content_html;
        inner.appendChild(prose);
      } else if (doc.markdown) {
        // A SELF-CARRIED packet's prose is NOT server-rendered and NOT
        // in the same trust domain — it is whatever the author put in
        // step metadata. The host's escape-first renderer
        // (window.__boss_markdown, web-kit's renderMarkdown) makes it
        // READABLE without trusting it: every character is escaped
        // before any tag the renderer emits, and hrefs admit only
        // http(s)/relative (2244db9e — "showed special markdown
        // characters"). Absent the host renderer (older SPA, tests),
        // the preserved-text fallback stands.
        const render = window.__boss_markdown;
        if (typeof render === 'function') {
          const prose = h('div', { className: 'srd-doc-inner-md' });
          prose.innerHTML = render(doc.markdown);
          inner.appendChild(prose);
        } else {
          const prose = h('pre', { className: 'srd-rawmd' });
          prose.textContent = doc.markdown;
          inner.appendChild(prose);
        }
      }
      docPane.appendChild(inner);
      bodyDiv.appendChild(docPane);

      // ---- Pane 2: the decisions, sticky beside the reading. ----
      const rail = h('div', { className: 'srd-rail' });
      if (questions.length === 0) {
        rail.appendChild(
          h(
            'div',
            { className: 'srd-empty' },
            'No open questions in this doc — it is ready to mark reviewed.',
          ),
        );
        bodyDiv.appendChild(rail);
        return;
      }
      rail.appendChild(
        h('div', { className: 'srd-rail-title' }, `Decisions (${questions.length})`),
      );
      questions.forEach((q) => {
        const addressed = resolutionFor(q.anchor).trim().length > 0;
        const ta = h('textarea', {
          rows: 4,
          placeholder: 'Record the decision, deferral, or rationale…',
          disabled: isDone,
          value: resolutionFor(q.anchor),
        });
        const card = h(
          'div',
          { className: `srd-q ${addressed ? 'is-addressed' : ''}` },
          h(
            'div',
            { className: 'srd-q-head' },
            h('span', { className: 'srd-anchor' }, q.anchor),
            h('span', { className: 'srd-q-title' }, q.title),
          ),
          (() => {
            if (q.body_html) {
              const b = h('div', { className: 'srd-q-body' });
              b.innerHTML = q.body_html;
              return b;
            }
            return q.body_md ? h('div', { className: 'srd-q-body' }, q.body_md) : null;
          })(),
          // The proposal, offered rather than applied (David,
          // 2026-08-14): "give you the ability to populate the
          // resolution but I have to still click the button ...
          // basically just authorizing you to copy and paste on my
          // behalf." So the draft sits here with a button, and the
          // resolution box stays empty until he presses it. Filling
          // the box directly would make every question count as
          // answered on page load, and Complete is gated on that
          // count — one stray click would then record decisions
          // nobody read.
          //
          // Nothing about this is recorded on the packet: the
          // proposal is the doc's own text, and what gets stored is
          // the resolution he committed.
          proposalBlock(q, (text) => {
            ta.value = text;
            setResolution(q.anchor, text);
            card.classList.add('is-addressed');
            ta.focus();
          }),
          h('label', { className: 'srd-label' }, 'Resolution'),
          ta,
        );
        // Toggle the addressed accent live, without re-rendering the
        // rail — a full re-render would blur the textarea mid-sentence.
        ta.addEventListener('input', (e) => {
          setResolution(q.anchor, e.target.value);
          card.classList.toggle('is-addressed', e.target.value.trim().length > 0);
        });
        rail.appendChild(card);
      });
      bodyDiv.appendChild(rail);
    }

    function renderActions() {
      actionsDiv.replaceChildren();
      if (saveError) {
        actionsDiv.appendChild(
          h(
            'div',
            { className: 'srd-error' },
            `Save failed: ${saveError}`,
          ),
        );
      }
      // A doc that failed to LOAD must not offer completion: with
      // `questions` still empty the gate below reads as "no questions"
      // and renders "Mark reviewed" on top of the error — reviewing a
      // doc nobody has seen (found by the 6f40b23f harness).
      if (loadError) return;
      if (isDone) return;
      const saveBtn = h(
        'button',
        { className: 'step-btn', disabled: saving },
        'Save progress',
      );
      saveBtn.addEventListener('click', () => save(false));
      actionsDiv.appendChild(saveBtn);
      if (allAnswered() || questions.length === 0) {
        const doneBtn = h(
          'button',
          { className: 'step-btn step-btn-primary', disabled: saving },
          questions.length === 0
            ? 'Mark reviewed (no questions)'
            : 'All addressed — complete review',
        );
        doneBtn.addEventListener('click', () => save(true));
        actionsDiv.appendChild(doneBtn);
      } else if (questions.length > 0) {
        actionsDiv.appendChild(
          h(
            'span',
            { className: 'step-review-gate-hint' },
            `Complete is gated on every question having a resolution (${answeredCount()}/${questions.length} done).`,
          ),
        );
      }
    }

    async function persistPendingDecisions() {
      // Mirror each non-empty resolution to /api/design/pending-decisions
      // so the existing flush-jobs path can extract them to ADRs. We
      // POST one at a time — the endpoint is upsert-style.
      // PendingDecisionInput wants {doc_path, anchor, kind, resolution}.
      // The old body sent `proposal` with no kind — a 422 this catch
      // swallowed, so flush-jobs always saw zero pending decisions.
      //
      // `kind` is now a fact rather than a constant. It used to be
      // hardcoded to override because no question ever carried a
      // proposal to accept (the parser looked for a spelling the corpus
      // does not use), which made the accept/override split carry no
      // information at all. It is derived from what the reviewer
      // submitted: identical to the doc's proposal means he took it,
      // anything else means he wrote his own. That is a claim about his
      // text, not about who drafted it — nothing here records that a
      // proposal was pre-filled.
      const proposalFor = (anchor) => {
        const q = questions.find((x) => x.anchor === anchor);
        return q && typeof q.proposal === 'string' ? q.proposal.trim() : '';
      };
      const writes = resolutions
        .filter((r) => r.decision.trim().length > 0)
        .map((r) =>
          fetch('/api/design/pending-decisions', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({
              doc_path: docPath,
              anchor: r.anchor,
              kind: r.decision.trim() === proposalFor(r.anchor) ? 'accept' : 'override',
              resolution: r.decision,
            }),
          }),
        );
      const results = await Promise.allSettled(writes);
      const failed = results.filter((r) => r.status === 'rejected' || (r.value && !r.value.ok));
      if (failed.length > 0) {
        // Don't block step save on a pending-decision write failure;
        // the resolution is still persisted on the step itself.
        console.warn('[review-design] pending-decisions writes failed:', failed.length);
      }
    }

    async function putStep(status, metadata) {
      const r = await fetch(`/api/jobs/${jobId}/steps/${step.id}`, {
        method: 'PUT',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ ...step, job_id: jobId, status, metadata }),
      });
      if (!r.ok) throw new Error(`step save HTTP ${r.status}: ${await r.text()}`);
    }

    async function save(autoComplete) {
      saving = true;
      saveError = null;
      renderActions();
      try {
        if (!selfCarried) await persistPendingDecisions();
        const completing = autoComplete && (allAnswered() || questions.length === 0);
        const workingStatus = step.status === 'pending' ? 'active' : step.status;
        const finalMeta = { ...step.metadata, doc_path: docPath, resolutions };

        // 1. Persist the FINAL shape first (title + metadata are what
        //    sign-off stamps attest — a stamp taken before the last
        //    metadata write goes stale and the completion 409s).
        await putStep(workingStatus, finalMeta);

        if (completing) {
          // 2. Stamp every required sign-off role in the step's now-
          //    final shape. Policy gates each on `step-signoff:<role>`
          //    — a 403 here means the signed-in user lacks that
          //    authority, and we SAY so instead of silently dropping
          //    it (the pre-fix flow swallowed the completion 409 and
          //    "Mark reviewed" appeared to do nothing).
          for (const role of step.sign_offs_required || []) {
            const r = await fetch(
              `/api/jobs/${jobId}/steps/${step.id}/sign-offs`,
              {
                method: 'POST',
                headers: { 'Content-Type': 'application/json' },
                body: JSON.stringify({ role }),
              },
            );
            if (!r.ok) {
              throw new Error(
                `sign-off as ${role} failed (HTTP ${r.status}): ${await r.text()}`,
              );
            }
          }
          // 3. Complete with the identical metadata the stamps attest.
          await putStep('completed', finalMeta);
        }
        onUpdate();
      } catch (e) {
        saveError = e instanceof Error ? e.message : String(e);
      } finally {
        saving = false;
        renderActions();
      }
    }

    async function load() {
      // SELF-CARRIED FIRST. When the step brings its own questions,
      // this plugin needs no docs-api and no file on deployed main —
      // the packet IS the doc.
      //
      // That is the whole point. The fetch below carries a 404 message
      // apologising that "review Jobs are instant data but docs ride
      // trains, so a review can exist before its doc reaches deployed
      // main" — which is a fair description of a review protocol that
      // cannot start until the thing being reviewed has already
      // shipped. David, 2026-08-16: "our lack of good protocol around
      // design docs, and the plumbing being broken too, is causing
      // major slowdowns in my design review handling speed."
      //
      // Backward compatible on purpose: every existing
      // design-doc-review Job has no `questions` key and takes the
      // fetch path exactly as before. Nothing in flight changes.
      const carried = step.metadata && step.metadata.questions;
      if (Array.isArray(carried) && carried.length > 0) {
        selfCarried = true;
        questions = carried.map((q, i) => ({
          anchor: String(q.anchor || `Q${i + 1}`),
          title: String(q.title || q.question || ''),
          proposal: typeof q.proposal === 'string' ? q.proposal : '',
          body: typeof q.body === 'string' ? q.body : '',
        }));
        // THE PROSE MAY BE ON EITHER BAG, and this reads both.
        //
        // The questions must live on the STEP — that is what makes the
        // packet self-carried. The prose has no such requirement, and
        // an author who puts `markdown` in the Job's metadata (the
        // natural place for "the document this packet is about") got a
        // review surface with questions and an EMPTY doc pane. That
        // shipped: David reviewed a design on 2026-08-16 seeing only
        // the questions, and answered four of them blind before asking
        // whether the content side was supposed to be empty.
        //
        // Falling back is the right shape rather than a workaround.
        // "Which metadata bag" is exactly the kind of detail that will
        // keep being got wrong, and the cost of guessing wrong should
        // be nothing rather than a silently unreadable review.
        const sm = step.metadata || {};
        doc = {
          title: String(sm.title || docPath || 'Design doc'),
          content_html: null,
          markdown: String(sm.markdown || ''),
        };
        if (!doc.markdown) {
          try {
            const jr = await fetch(`/api/jobs/${jobId}`, {
              headers: { accept: 'application/json' },
            });
            if (jr.ok) {
              const job = await jr.json();
              const jm = (job && job.metadata) || {};
              doc.markdown = String(jm.markdown || '');
              if (!sm.title && jm.title) doc.title = String(jm.title);
            }
          } catch (_) {
            // A packet with questions and no readable prose is still
            // reviewable; leave the doc pane empty rather than fail
            // the whole surface.
          }
        }
        renderHeader();
        renderBody();
        renderProgress();
        renderActions();
        return;
      }
      // A DOC WITH NO OPEN QUESTIONS IS STILL A DOC.
      //
      // The self-carried branch above requires a NON-EMPTY questions
      // array, so a design-doc packet whose prose is carried inline but
      // which has no open questions — settled, or never had any — fell
      // through to the error below and reported "nothing to review"
      // while its markdown sat in the very metadata this plugin had
      // already read. c4b7c904 is that packet: step metadata carrying
      // `markdown` and `title`, no questions, no doc_path.
      //
      // This is the third time this file has had to learn that the
      // content may be somewhere it did not look. First the doc pane
      // was empty because the prose was on the other metadata bag
      // (2026-08-16, four questions answered blind). Then the questions
      // had to be allowed to ride the packet at all. Now: having
      // questions is not what makes a doc reviewable — having the doc
      // is. Reading it and settling it is a review.
      const inlineMarkdown = String((step.metadata && step.metadata.markdown) || '');
      if (!docPath && inlineMarkdown) {
        selfCarried = true;
        questions = [];
        doc = {
          title: String((step.metadata && step.metadata.title) || 'Design doc'),
          content_html: null,
          markdown: inlineMarkdown,
        };
        renderHeader();
        renderBody();
        renderProgress();
        renderActions();
        return;
      }
      if (!docPath) {
        // LAST RESORT: the questions and prose may be on the JOB
        // metadata rather than the step. That is the natural place an
        // author puts them, and the same guess the markdown fallback
        // above already forgives — "the cost of guessing wrong should be
        // nothing rather than a silently unreadable review". acedf981
        // and the `[sim] decision-routing probe` packets hit exactly
        // this: a design-doc filed with its content on the job reached
        // review as an empty step and dead-ended here. Answers still
        // write to the step, so the review stays self-carried.
        try {
          const jr = await fetch(`/api/jobs/${jobId}`, {
            headers: { accept: 'application/json' },
          });
          if (jr.ok) {
            const jm = ((await jr.json()) || {}).metadata || {};
            const jq = Array.isArray(jm.questions) ? jm.questions : [];
            const jmd = String(jm.markdown || '');
            if (jq.length > 0 || jmd) {
              selfCarried = true;
              questions = jq.map((q, i) => ({
                anchor: String(q.anchor || `Q${i + 1}`),
                title: String(q.title || q.question || ''),
                proposal: typeof q.proposal === 'string' ? q.proposal : '',
                body: typeof q.body === 'string' ? q.body : '',
              }));
              doc = {
                title: String(jm.title || 'Design doc'),
                content_html: null,
                markdown: jmd,
              };
              renderHeader();
              renderBody();
              renderProgress();
              renderActions();
              return;
            }
          }
        } catch (_) {
          // Fall through to the honest error below.
        }
        loadError =
          'this step carries neither metadata.questions, metadata.markdown, nor ' +
          'metadata.doc_path, and the job carries none either — nothing to review';
        renderBody();
        renderProgress();
        renderActions();
        return;
      }
      try {
        const r = await fetch(`/api/design/docs/${docPath}`);
        if (r.status === 404) {
          // The honest miss (2e6dfde7): review Jobs are instant data
          // but docs ride trains, so a review can exist before its
          // doc reaches deployed main. A bare 404 read as a dead end
          // to the first operator who hit it; say what is actually
          // happening and when it resolves.
          loadError =
            `${docPath} is not on the deployed main yet — docs ride ` +
            `release trains, and this review was opened ahead of its ` +
            `doc's landing. It becomes reviewable when the train ` +
            `carrying the doc merges and deploys. If this persists ` +
            `after a landing, the doc may have been REJECTED at ` +
            `reindex (stray questions outside '## Open questions') — ` +
            `the rejection reason is recorded at /system/design.`;
          renderBody();
          renderProgress();
          renderActions();
          return;
        }
        if (!r.ok) throw new Error(`HTTP ${r.status}: ${await r.text()}`);
        // The other honest miss (6f40b23f): a front that does not
        // route /api/design/* answers 200 with a ZERO-BYTE body — the
        // docs service runs on the operator instance only. Left to
        // r.json() this rendered as a JSON parse error, which reads
        // like a broken doc rather than an absent service.
        const raw = await r.text();
        if (!raw.trim()) {
          loadError =
            `this instance does not serve the docs API (an empty reply ` +
            `for ${docPath}) — the docs service runs on the operator ` +
            `instance only. Reviews spawned since 2026-08-18 carry ` +
            `their questions in the packet and never need this fetch; ` +
            `this older packet carries only a pointer. Open it on the ` +
            `operator instance, or re-spawn the review to get a ` +
            `self-carried packet.`;
          renderBody();
          renderProgress();
          renderActions();
          return;
        }
        const detail = JSON.parse(raw);
        doc = detail;
        questions = Array.isArray(detail.questions) ? detail.questions : [];
      } catch (e) {
        loadError = e instanceof Error ? e.message : String(e);
      }
      renderBody();
      renderProgress();
      renderActions();
    }

    const root = h(
      'div',
      { className: 'step-surface step-review-design' },
      headerDiv,
      bodyDiv,
      actionsDiv,
    );

    injectStyles();
    renderHeader();
    renderProgress();
    renderBody();
    renderActions();
    container.appendChild(root);
    void load();

    return function cleanup() {
      root.remove();
    };
  }

  if (typeof window.__boss_register_step_plugin !== 'function') {
    console.error('[review-design-plugin] __boss_register_step_plugin not on window');
    return;
  }
  window.__boss_register_step_plugin('review-design', mount);
})();
