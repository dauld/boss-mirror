// correction-verdict.js — custom Step UX for the accept/reject gate of
// the correct-the-record Workflow.
//
// WHY THIS EXISTS. David, looking at four of these parked in his queue:
// "The UX for the corrections is hard for me to understand the question
// and trade-off." He was right, and the reason is structural rather than
// cosmetic. The step is a bare `task` whose card reads "Accept the
// correction" and offers a `verdict` field with two enum values. The
// things a person needs in order to answer — what was claimed, what was
// measured instead, how it was measured, and where the false claim is
// still sitting — all live on the PREVIOUS step's metadata, one packet
// modal and a JSON dump away. The question was on screen; the material
// to answer it was not.
//
// So this surface does one thing: put the claim and the measurement that
// contradicts it side by side, say plainly what each verdict causes, and
// then take the answer. Nothing here is new information — it is the same
// metadata the generic surface already had, arranged so the trade-off is
// the thing you see first.
//
// WHAT IT READS. The `evidence` step of the same Job carries
// `claim`, `measured`, `method` and `where`; the Job's own metadata
// carries `corrects` (the packet id being corrected) and
// `corrects_title`. The step props give us `step` and `jobId` only, so
// we fetch the Job to reach its siblings — one request, and the response
// already enriches each Job with its steps.
//
// WHAT IT WRITES. `metadata.verdict` = "accepted" | "unfounded", then
// completes the step. The two successors are gated on exactly that value
// (`applied` on accepted, `unfounded` on the other), so this field is
// the fork and there is nothing else to set.
//
// Plugin contract: window.__boss_register_step_plugin(kind, mount).
// Host calls mount(container, props) with { step, jobId, onUpdate }.

(function () {
  // ---------------------------------------------------------------
  // Self-contained styling, injected once, scoped under
  // `.step-correction-verdict`, written against core's CSS custom
  // properties with fallbacks so it follows the tenant's theme rather
  // than fighting it. Same reasoning as review-design.js: a plugin has
  // no business adding rules to core, and without this the surface
  // renders at browser defaults — full-width unmeasured prose, which
  // is the exact failure being fixed.
  // ---------------------------------------------------------------
  const STYLE_ID = 'boss-correction-verdict-styles';
  const STYLES = `
.step-correction-verdict { --scv-gap: 18px; max-width: 900px; }

.step-correction-verdict .scv-head {
  display: flex; align-items: baseline; gap: 12px; flex-wrap: wrap;
  padding-bottom: 10px; margin-bottom: var(--scv-gap);
  border-bottom: 1px solid var(--border, #e7e5e4);
}
.step-correction-verdict .scv-head h3 { margin: 0; font-size: 17px; flex: 1 1 auto; }
.step-correction-verdict .scv-corrects {
  font-size: 12px; color: var(--text-dim, #78716c); white-space: nowrap;
}
.step-correction-verdict .scv-corrects a { color: var(--accent, #2563eb); }

/* The comparison. Two panes on a wide surface, stacked on a narrow one.
   Deliberately NOT a diff: these are two assertions about the world, and
   the reader's job is to judge which one is true. */
.step-correction-verdict .scv-compare {
  display: grid; gap: 12px; margin-bottom: var(--scv-gap);
  grid-template-columns: 1fr 1fr;
}
@media (max-width: 720px) {
  .step-correction-verdict .scv-compare { grid-template-columns: 1fr; }
}
.step-correction-verdict .scv-pane {
  border: 1px solid var(--border, #e7e5e4); border-radius: 6px;
  padding: 12px 14px; background: var(--bg-raised, transparent);
}
.step-correction-verdict .scv-pane h4 {
  margin: 0 0 8px; font-size: 11px; text-transform: uppercase;
  letter-spacing: 0.6px; font-weight: 700;
}
.step-correction-verdict .scv-claim { border-left: 3px solid #b91c1c; }
.step-correction-verdict .scv-claim h4 { color: #b91c1c; }
.step-correction-verdict .scv-measured { border-left: 3px solid #15803d; }
.step-correction-verdict .scv-measured h4 { color: #15803d; }
.step-correction-verdict .scv-body {
  margin: 0; font-size: 13px; line-height: 1.5;
  white-space: pre-wrap; word-break: break-word;
}
.step-correction-verdict .scv-missing {
  font-style: italic; color: var(--text-dim, #78716c);
}

/* Method and location: supporting detail, collapsed by default so the
   comparison above stays the first thing read. */
.step-correction-verdict .scv-detail {
  border: 1px solid var(--border, #e7e5e4); border-radius: 6px;
  padding: 8px 14px; margin-bottom: 12px;
}
.step-correction-verdict .scv-detail > summary {
  cursor: pointer; font-size: 12px; font-weight: 600;
  color: var(--text-dim, #78716c);
}
.step-correction-verdict .scv-detail .scv-body { margin-top: 10px; }

/* The choice. Each option states its consequence, because "accepted"
   and "unfounded" do not say what happens next and that is the part
   that was missing. */
.step-correction-verdict .scv-choice { display: grid; gap: 10px; margin-bottom: var(--scv-gap); }
.step-correction-verdict .scv-opt {
  display: flex; gap: 10px; align-items: flex-start;
  border: 1px solid var(--border, #e7e5e4); border-radius: 6px;
  padding: 12px 14px; cursor: pointer;
}
.step-correction-verdict .scv-opt:hover { border-color: var(--accent, #2563eb); }
.step-correction-verdict .scv-opt.scv-on {
  border-color: var(--accent, #2563eb);
  box-shadow: inset 0 0 0 1px var(--accent, #2563eb);
}
.step-correction-verdict .scv-opt input { margin-top: 3px; flex: 0 0 auto; }
.step-correction-verdict .scv-opt-label { font-size: 14px; font-weight: 600; margin: 0 0 3px; }
.step-correction-verdict .scv-opt-what { font-size: 12px; color: var(--text-dim, #78716c); margin: 0; line-height: 1.45; }

.step-correction-verdict .scv-actions { display: flex; align-items: center; gap: 12px; }
.step-correction-verdict .scv-actions button {
  font: inherit; font-size: 13px; font-weight: 600;
  padding: 7px 18px; border-radius: 5px; cursor: pointer;
  border: 1px solid var(--accent, #2563eb);
  background: var(--accent, #2563eb); color: #fff;
}
.step-correction-verdict .scv-actions button[disabled] { opacity: 0.5; cursor: not-allowed; }
.step-correction-verdict .scv-err { color: #b91c1c; font-size: 12px; }
.step-correction-verdict .scv-done {
  font-size: 13px; padding: 10px 14px; border-radius: 6px;
  border: 1px solid var(--border, #e7e5e4);
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

  /// A field that should be there but is not renders as a visible
  /// absence rather than an empty box. An evidence step is supposed to
  /// carry all four; a blank one means the correction was filed without
  /// its evidence, and that is a reason to send it back, not something
  /// to hide behind whitespace.
  function body(text, missingLabel) {
    if (text != null && String(text).trim() !== '') {
      return `<p class="scv-body">${esc(text)}</p>`;
    }
    return `<p class="scv-body scv-missing">${esc(missingLabel)}</p>`;
  }

  const OPTIONS = [
    {
      value: 'accepted',
      label: 'Accept — the correction is right',
      what:
        'The claim was false and the measurement stands. Opens "Land it where ' +
        'the claim lives", which is where the doc or note actually gets fixed. ' +
        'The packet stays open until that lands.',
    },
    {
      value: 'unfounded',
      label: 'Reject — the original claim held up',
      what:
        'The correction is wrong, or its evidence does not support it. Closes ' +
        'this packet with the original claim intact and nothing edited.',
    },
  ];

  function mount(container, { step, jobId, onUpdate }) {
    injectStyles();

    let verdict = (step.metadata && step.metadata.verdict) || null;
    let saving = false;
    let saveError = null;
    let evidence = null;
    let job = null;

    const root = document.createElement('div');
    root.className = 'step-correction-verdict';
    container.appendChild(root);

    const terminal = step.status === 'completed' || step.status === 'skipped';

    function render() {
      if (terminal) {
        root.innerHTML =
          `<div class="scv-done">Verdict recorded: <strong>${esc(verdict || 'none')}</strong>.</div>`;
        return;
      }

      const claim = evidence && evidence.metadata ? evidence.metadata.claim : null;
      const measured = evidence && evidence.metadata ? evidence.metadata.measured : null;
      const method = evidence && evidence.metadata ? evidence.metadata.method : null;
      const where = evidence && evidence.metadata ? evidence.metadata.where : null;
      const jm = (job && job.metadata) || {};
      const correctsId = jm.corrects;
      const correctsTitle = jm.corrects_title;

      root.innerHTML = `
        <div class="scv-head">
          <h3>Is this correction right?</h3>
          ${
            correctsId
              ? `<span class="scv-corrects">correcting
                   <a href="/ux/jobs/${esc(correctsId)}">${esc(correctsTitle || correctsId)}</a>
                 </span>`
              : ''
          }
        </div>

        <div class="scv-compare">
          <div class="scv-pane scv-claim">
            <h4>What was claimed</h4>
            ${body(claim, 'No claim recorded on the evidence step — send this back rather than judging it.')}
          </div>
          <div class="scv-pane scv-measured">
            <h4>What was measured</h4>
            ${body(measured, 'No measurement recorded — there is nothing here to accept.')}
          </div>
        </div>

        <details class="scv-detail">
          <summary>How it was measured</summary>
          ${body(method, 'No method recorded.')}
        </details>
        <details class="scv-detail">
          <summary>Where the claim still lives</summary>
          ${body(where, 'Not recorded — accepting this leaves nobody knowing what to edit.')}
        </details>

        <div class="scv-choice">
          ${OPTIONS.map(
            (o) => `
            <label class="scv-opt${verdict === o.value ? ' scv-on' : ''}">
              <input type="radio" name="scv-verdict" value="${o.value}"
                     ${verdict === o.value ? 'checked' : ''} />
              <span>
                <p class="scv-opt-label">${esc(o.label)}</p>
                <p class="scv-opt-what">${esc(o.what)}</p>
              </span>
            </label>`,
          ).join('')}
        </div>

        <div class="scv-actions">
          <button type="button" ${verdict && !saving ? '' : 'disabled'}>
            ${saving ? 'Recording…' : 'Record verdict'}
          </button>
          ${saveError ? `<span class="scv-err">${esc(saveError)}</span>` : ''}
        </div>
      `;

      root.querySelectorAll('input[name="scv-verdict"]').forEach((input) => {
        input.addEventListener('change', () => {
          verdict = input.value;
          render();
        });
      });
      const btn = root.querySelector('.scv-actions button');
      if (btn) btn.addEventListener('click', save);
    }

    async function save() {
      if (!verdict || saving) return;
      saving = true;
      saveError = null;
      render();
      try {
        // 1. Merge ONLY the key this surface owns. The old idiom sent
        //    the page-load snapshot plus `verdict` through the PUT, and
        //    PUT metadata is replaced WHOLESALE — any key another
        //    writer added after this page loaded was silently erased
        //    (the lost update the step metadata PATCH exists to
        //    retire). The server merges against the row as it stands
        //    and preserves every key this body does not name.
        const pr = await fetch(`/api/jobs/${jobId}/steps/${step.id}/metadata`, {
          method: 'PATCH',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ verdict }),
        });
        if (!pr.ok) throw new Error(`metadata merge HTTP ${pr.status}: ${await pr.text()}`);
        // 2. The PATCH answers 204 with no body, and the completion PUT
        //    below still replaces metadata wholesale — so read the
        //    post-merge row back and complete with THAT, never the
        //    snapshot. (No single-step GET exists; the job's steps
        //    list is the read the API offers.)
        const lr = await fetch(`/api/jobs/${jobId}/steps`);
        if (!lr.ok) throw new Error(`step read-back HTTP ${lr.status}: ${await lr.text()}`);
        const stepsNow = await lr.json();
        const fresh = Array.isArray(stepsNow)
          ? stepsNow.find((s) => s.id === step.id)
          : null;
        if (!fresh) throw new Error('step read-back: step missing from its own job');
        // 3. Complete with the true final shape — the fresh row's
        //    metadata rides the body verbatim.
        const r = await fetch(`/api/jobs/${jobId}/steps/${step.id}`, {
          method: 'PUT',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify(
            Object.assign({}, fresh, { job_id: jobId, status: 'completed' }),
          ),
        });
        // Read the code. A swallowed non-2xx here would leave the
        // surface looking saved while the fork never moved, which is
        // the failure mode that cost a session on 2026-08-17.
        if (!r.ok) throw new Error(`HTTP ${r.status}: ${await r.text()}`);
        onUpdate();
      } catch (e) {
        saveError = e && e.message ? e.message : String(e);
        saving = false;
        render();
      }
    }

    // First paint immediately from what the host already gave us, so the
    // surface is never blank while the Job round-trips; the comparison
    // fills in when it lands.
    render();

    fetch(`/api/jobs/${jobId}`)
      .then((r) => (r.ok ? r.json() : null))
      .then((j) => {
        if (!j) return;
        job = j;
        const steps = Array.isArray(j.steps) ? j.steps : [];
        // Find the sibling by its spec slug rather than by position or
        // title: slugs are the stable identifier a Workflow edit keeps,
        // and matching on a step kind is what infra/lint/
        // no-step-kind-match.sh exists to stop.
        evidence =
          steps.find((s) => s.spec_slug === 'evidence') ||
          steps.find((s) => s.metadata && s.metadata.claim != null) ||
          null;
        render();
      })
      .catch(() => {
        // The comparison is the point, so say so rather than rendering
        // a confident-looking empty surface.
        saveError = 'Could not load the correction’s evidence — reload before judging it.';
        render();
      });

    return function cleanup() {
      root.remove();
    };
  }

  if (typeof window.__boss_register_step_plugin !== 'function') {
    console.error('[correction-verdict-plugin] __boss_register_step_plugin not on window');
    return;
  }
  window.__boss_register_step_plugin('correction-verdict', mount);
})();
