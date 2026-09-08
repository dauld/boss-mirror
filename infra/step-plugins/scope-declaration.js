// scope-declaration.js — custom Step UX for the `scope` step of the
// ship-a-change Workflow: the moment a person says what a change
// contains and what it deliberately leaves out.
//
// WHY THIS EXISTS. David, holding one of these: "I don't know how to
// input my decision within this UX / I think the wrong step UX is
// showing." Both halves were fair. The step is a bare `task` with two
// required string fields, so the generic surface renders it as two
// unlabelled textareas named `summary` and `excludes` — no prompt, no
// example, nothing saying that `excludes` is the field the whole step
// exists for. A person meeting it for the first time cannot tell what
// is being asked, and "the wrong UX is showing" is the right reading of
// a surface that says nothing about the decision it is taking.
//
// It is also the most-used human step in the car protocol: every car
// starts here. A surface used that often should teach the protocol
// rather than assume it.
//
// WHAT IT ASKS, AND WHY IN THIS ORDER.
//
//   summary  — what this car DOES. One sentence. It is the boundary
//              you will defend at review, so it is written before the
//              work rather than reconstructed after it.
//
//   excludes — what it deliberately does NOT do, and why. This is the
//              protocol's honesty valve. registry.rs says it plainly:
//              "naming what you are not doing is the act that keeps a
//              change small, and a field nobody has to fill in would be
//              filled in never." The branch that introduced
//              ship-a-change was itself the evidence — one PR carrying
//              a guest sign-in, a dispatcher fix, a ledger determinism
//              fix and two new surfaces, because nothing ever asked
//              where it should have been cut. So the prompt asks for
//              the thing you could plausibly have swept in, and the
//              reason you are not.
//
// WHAT IT READS. The Job, once: its Subject is a `custom` Subject whose
// id IS the branch, so the surface can show the car it is scoping
// instead of leaving the author to remember. `metadata.backlog_item` is
// the packet this change answers — the request that bounds it — and
// links through when present. If the `gate` step is already completed
// (a scope declared or edited after the work, which happens), its
// receipt and its `verified` prose are shown collapsed: what the branch
// actually turned out to contain is the best available check on what
// you are about to claim it contains.
//
// WHAT IT WRITES. `metadata.summary` and `metadata.excludes`, then
// completes the step. Nothing else — the completion contract is the
// step row's own authored `fields` (both required at done), and the
// surface only reflects that rule rather than inventing one of its own.
//
// REGISTERED ON ITS OWN KIND, `scope-declaration`, NOT on `task`.
// Plugins register by step kind and the SPA mounts by kind
// (apps/web/src/steps/StepSurface.svelte prefers an active plugin over
// the built-in surface), so a plugin on `task` would hijack every task
// step on the platform. Same reasoning, same precedent as schema 146
// (correction-verdict). The Rust StepRegistry does not need to learn
// the kind either: `validate_metadata` is permissive for kinds it does
// not know, and the gate still comes from the step's authored fields.
//
// ACTIVATION IS A FOLLOW-UP. ship-a-change's `scope` step is still
// `kind = "task"`, so this surface is inert until a Workflow v2
// repoints it — a registry write, no deploy.
//
// Plugin contract: window.__boss_register_step_plugin(kind, mount).
// Host calls mount(container, props) with { step, jobId, onUpdate }.

(function () {
  // ---------------------------------------------------------------
  // Self-contained styling, injected once, scoped under
  // `.step-scope-declaration`. A plugin has no business adding rules to
  // core, and without this the surface renders at browser defaults.
  //
  // Written against core's tokens, and the FALLBACKS ARE THE APP'S
  // PALETTE — dark grounds, hairline borders, square corners. The
  // sibling bundles fall back to light stone hex, which paints a white
  // card on VOID the moment a token fails to resolve. There is one
  // palette (apps/web/src/styles.css :root); a fallback is a place to
  // restate it, not a place to smuggle in a second one.
  // ---------------------------------------------------------------
  const STYLE_ID = 'boss-scope-declaration-styles';
  const STYLES = `
.step-scope-declaration { --ssd-gap: 18px; max-width: 820px; }

.step-scope-declaration .ssd-head {
  display: flex; align-items: baseline; gap: 12px; flex-wrap: wrap;
  padding-bottom: 10px; margin-bottom: 12px;
  border-bottom: 1px solid var(--border, #2a3138);
}
.step-scope-declaration .ssd-head h3 { margin: 0; font-size: 17px; flex: 1 1 auto; }

/* What this car is, from the packet itself — so the boundary is
   declared against a branch and a request, not from memory. */
.step-scope-declaration .ssd-context {
  display: flex; flex-wrap: wrap; gap: 6px 18px;
  font-size: 12px; color: var(--text-dim, #7a838c);
  margin-bottom: var(--ssd-gap);
}
.step-scope-declaration .ssd-context b {
  font-weight: 600; color: var(--text, #e8ecef);
  font-family: var(--font-mono, ui-monospace, monospace);
}
.step-scope-declaration .ssd-context a { color: var(--accent, #5fd4a8); }

.step-scope-declaration .ssd-field { margin-bottom: var(--ssd-gap); }
.step-scope-declaration .ssd-field > label {
  display: block; font-size: 14px; font-weight: 600; margin-bottom: 4px;
}
/* The prompt is the surface. Two textareas with field names above them
   is what this replaces. */
.step-scope-declaration .ssd-why {
  margin: 0 0 8px; font-size: 12px; line-height: 1.5;
  color: var(--text-dim, #7a838c);
}
.step-scope-declaration .ssd-field textarea {
  width: 100%; box-sizing: border-box; font: inherit; font-size: 13px;
  line-height: 1.5; padding: 9px 11px; resize: vertical;
  border: 1px solid var(--border, #2a3138);
  border-radius: var(--radius, 0);
  background: var(--bg, #0d1014); color: var(--text, #e8ecef);
}
.step-scope-declaration .ssd-field textarea:focus {
  outline: 2px solid var(--accent, #5fd4a8); outline-offset: 1px;
}
.step-scope-declaration .ssd-eg {
  margin: 6px 0 0; font-size: 12px; font-style: italic;
  color: var(--text-dim, #7a838c);
}

/* The gate's own account of the branch, when there is one. Collapsed:
   it is a check on the claim, not the question being asked. */
.step-scope-declaration .ssd-gate {
  border: 1px solid var(--border, #2a3138);
  border-radius: var(--radius, 0);
  padding: 8px 14px; margin-bottom: var(--ssd-gap);
}
.step-scope-declaration .ssd-gate > summary {
  cursor: pointer; font-size: 12px; font-weight: 600;
  color: var(--text-dim, #7a838c);
}
.step-scope-declaration .ssd-gate pre {
  margin: 10px 0 0; padding: 10px; overflow: auto; max-height: 260px;
  font-size: 11px; line-height: 1.45;
  font-family: var(--font-mono, ui-monospace, monospace);
  background: var(--card, #12161c);
  border: 1px solid var(--border, #2a3138);
}
.step-scope-declaration .ssd-gate p { font-size: 12px; line-height: 1.5; }

.step-scope-declaration .ssd-actions { display: flex; align-items: center; gap: 12px; }
.step-scope-declaration .ssd-actions button {
  font: inherit; font-size: 13px; font-weight: 600;
  padding: 7px 18px; border-radius: var(--radius, 0); cursor: pointer;
  border: 1px solid var(--accent, #5fd4a8);
  background: var(--accent, #5fd4a8); color: var(--bg, #0d1014);
}
.step-scope-declaration .ssd-actions button[disabled] { opacity: 0.5; cursor: not-allowed; }
.step-scope-declaration .ssd-hint { font-size: 12px; color: var(--text-dim, #7a838c); }
.step-scope-declaration .ssd-err { color: var(--err, #e2685c); font-size: 12px; }

/* Declared: the record, read back in the same two halves it was asked in. */
.step-scope-declaration .ssd-done {
  border: 1px solid var(--border, #2a3138);
  border-radius: var(--radius, 0); padding: 12px 14px;
}
.step-scope-declaration .ssd-done h4 {
  margin: 0 0 4px; font-size: 11px; text-transform: uppercase;
  letter-spacing: 0.6px; color: var(--text-dim, #7a838c);
}
.step-scope-declaration .ssd-done p {
  margin: 0 0 12px; font-size: 13px; line-height: 1.5; white-space: pre-wrap;
}
.step-scope-declaration .ssd-done p:last-child { margin-bottom: 0; }
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

  function mount(container, { step, jobId, onUpdate }) {
    injectStyles();

    const md = step.metadata || {};
    let summary = md.summary == null ? '' : String(md.summary);
    let excludes = md.excludes == null ? '' : String(md.excludes);
    let saving = false;
    let saveError = null;
    let job = null;
    let gate = null;

    const root = document.createElement('div');
    root.className = 'step-scope-declaration';
    container.appendChild(root);

    const terminal = step.status === 'completed' || step.status === 'skipped';

    function contextLine() {
      // The Subject id IS the branch for a ship-a-change Job (see
      // registry.rs: "a `custom` Subject whose id is the branch"). A
      // Job that carries an explicit branch in metadata wins, because
      // an explicit statement beats an inference.
      const jm = (job && job.metadata) || {};
      const branch = jm.branch || (job && job.subject ? job.subject.id : null);
      const backlogId = jm.backlog_item;
      const backlogText = jm.backlog_text;
      const bits = [];
      if (branch) bits.push(`branch <b>${esc(branch)}</b>`);
      if (backlogId) {
        bits.push(`answers <a href="/ux/jobs/${esc(backlogId)}">${esc(backlogId)}</a>`);
      } else if (backlogText) {
        bits.push(`answers ${esc(backlogText)}`);
      }
      if (!bits.length) return '';
      return `<div class="ssd-context">${bits.map((b) => `<span>${b}</span>`).join('')}</div>`;
    }

    /// The gate's receipt, when the gate has already run on this
    /// branch. Only rendered from a COMPLETED gate step: a receipt from
    /// a step still in flight describes a run that may not be the one
    /// that ends up mattering.
    function gateBlock() {
      if (!gate || !gate.metadata) return '';
      const gm = gate.metadata;
      const receipt = gm.receipt;
      const verified = gm.verified;
      if (!receipt && !verified) return '';
      return `
        <details class="ssd-gate">
          <summary>The gate has already run on this branch — what it recorded</summary>
          ${verified ? `<p>${esc(verified)}</p>` : ''}
          ${receipt ? `<pre>${esc(receipt)}</pre>` : ''}
        </details>`;
    }

    function render() {
      if (terminal) {
        root.innerHTML = `
          ${contextLine()}
          <div class="ssd-done">
            <h4>This car does</h4>
            <p>${esc(summary || '— nothing recorded —')}</p>
            <h4>It deliberately does not</h4>
            <p>${esc(excludes || '— nothing recorded —')}</p>
          </div>`;
        return;
      }

      const ready = summary.trim() !== '' && excludes.trim() !== '';
      root.innerHTML = `
        <div class="ssd-head">
          <h3>What is this change, and what is it not?</h3>
        </div>
        ${contextLine()}
        ${gateBlock()}

        <div class="ssd-field">
          <label for="ssd-summary">What this car DOES</label>
          <p class="ssd-why">
            One sentence naming the change. This is the boundary you will
            defend at review, written before the work rather than
            reconstructed after it.
          </p>
          <textarea id="ssd-summary" rows="3"
            placeholder="Fix the marketing-asset tag chips so the text is readable.">${esc(summary)}</textarea>
        </div>

        <div class="ssd-field">
          <label for="ssd-excludes">What it deliberately does NOT do — and why</label>
          <p class="ssd-why">
            Name the next thing you could plausibly have swept in, and the
            reason you are not. Naming what you are not doing is the act
            that keeps a change small; this is the field the step exists
            for, and the one place the protocol asks you to be honest
            about scope while it is still cheap.
          </p>
          <textarea id="ssd-excludes" rows="4"
            placeholder="Not sweeping the other pages' light-theme hex — a sibling car owns that file and two diffs there would collide.">${esc(excludes)}</textarea>
          <p class="ssd-eg">
            "Nothing" is almost never true. If it is, say what you
            considered and why it turned out to be in scope after all.
          </p>
        </div>

        <div class="ssd-actions">
          <button type="button" ${ready && !saving ? '' : 'disabled'}>
            ${saving ? 'Declaring…' : 'Declare the boundary'}
          </button>
          ${
            ready
              ? ''
              : '<span class="ssd-hint">Both halves are required to complete this step.</span>'
          }
          ${saveError ? `<span class="ssd-err">${esc(saveError)}</span>` : ''}
        </div>
      `;

      // Bind without re-rendering on every keystroke: a full re-render
      // would move the caret to the end of the box mid-sentence.
      const sEl = root.querySelector('#ssd-summary');
      const eEl = root.querySelector('#ssd-excludes');
      const btn = root.querySelector('.ssd-actions button');
      const hint = root.querySelector('.ssd-hint');
      function sync() {
        summary = sEl.value;
        excludes = eEl.value;
        const ok = summary.trim() !== '' && excludes.trim() !== '';
        btn.disabled = !ok || saving;
        if (hint) hint.style.display = ok ? 'none' : '';
      }
      sEl.addEventListener('input', sync);
      eEl.addEventListener('input', sync);
      btn.addEventListener('click', save);
    }

    async function save() {
      if (saving) return;
      if (summary.trim() === '' || excludes.trim() === '') return;
      saving = true;
      saveError = null;
      render();
      try {
        // 1. Merge ONLY the keys this surface owns. The old idiom sent
        //    the page-load snapshot plus these two through the PUT, and
        //    PUT metadata is replaced WHOLESALE — any key another
        //    writer added after this page loaded was silently erased
        //    (the lost update the step metadata PATCH exists to
        //    retire). The server merges against the row as it stands
        //    and preserves every key this body does not name.
        const pr = await fetch(`/api/jobs/${jobId}/steps/${step.id}/metadata`, {
          method: 'PATCH',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({
            summary: summary.trim(),
            excludes: excludes.trim(),
          }),
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
        // Read the code. A swallowed non-2xx leaves the surface looking
        // saved while the step never moved — the failure that cost a
        // session on 2026-08-17 (see correction-verdict.js).
        if (!r.ok) throw new Error(`HTTP ${r.status}: ${await r.text()}`);
        onUpdate();
      } catch (e) {
        saveError = e && e.message ? e.message : String(e);
        saving = false;
        render();
      }
    }

    // Paint from what the host already handed us, so the surface is
    // never blank while the Job round-trips; the context fills in when
    // it lands. The prompts do not depend on it.
    render();

    fetch(`/api/jobs/${jobId}`)
      .then((r) => (r.ok ? r.json() : null))
      .then((j) => {
        if (!j) return;
        job = j;
        const steps = Array.isArray(j.steps) ? j.steps : [];
        // By spec slug, not by kind: slugs are the stable identifier a
        // Workflow edit keeps, and matching on a step kind is what
        // infra/lint/no-step-kind-match.sh exists to stop.
        const g = steps.find((s) => s.spec_slug === 'gate');
        gate = g && g.status === 'completed' ? g : null;
        render();
      })
      .catch(() => {
        // The context is a help, not the question. A failed fetch drops
        // it silently rather than blocking a declaration the author can
        // make from what they already know.
      });

    return function cleanup() {
      root.remove();
    };
  }

  if (typeof window.__boss_register_step_plugin !== 'function') {
    console.error('[scope-declaration-plugin] __boss_register_step_plugin not on window');
    return;
  }
  window.__boss_register_step_plugin('scope-declaration', mount);
})();
