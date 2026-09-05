// sign-off.js v3 — the surface for a step that needs someone's name on
// a DECISION, showing them what they are deciding.
//
// v1 (car 884b85f4's train) rendered the stamp ceremony — the role
// roster, who signed when, stale-stamp 409s — and nothing else. That
// solved "Missing custom step UX" (b1aa1f5f) for ceremony steps and
// then, because a plugin evicts the platform surface for its whole
// KIND, blinded every decision-shaped sign-off in the system:
// 19db52de, David on his publish approval, "There is just a sign and
// complete button, which doesn't seem like much of a choice." The row
// was retired live on 2026-08-19; this version earns it back.
//
// What a sign-off step actually carries, all now rendered:
//   1. THE CASE — step.metadata.context_md, else the job's context_md,
//      else the job's filed message (the DecisionContext chain; a
//      plugin is exempt from the host panel, so it folds in here).
//   2. THE CONTRACT — step.fields declares required-at-done metadata
//      (publish-to-github v3 requires `approved`); v1 offered a
//      Complete that could only 400 against these, and did not show
//      the 400. Declared fields render as inputs, enums as selects,
//      and the decision buttons stay disabled until required ones are
//      filled — a button that exists only to produce an error teaches
//      the operator to distrust buttons (v1's own line, kept).
//   3. THE DECISION — Approve / Reject / Request changes, writing the
//      decision trio (`decision`, `decided_at`, `comment`) exactly as
//      the platform ApprovalSurface does, so protocol predicates read
//      one vocabulary regardless of which surface recorded it.
//   4. THE CEREMONY — v1's roster, verbatim in behavior: stamps are
//      collected per role, the step cannot complete while one is
//      outstanding, and a 409 surfaces the server's stale-roles text.
//
// Order on Approve/Reject: metadata lands first (a stamp attests the
// step's current shape, so the decision must be IN the shape), then
// the user's own stamp if their role is required and unsigned, then
// the completion — skipped, with a plain explanation, while other
// roles' signatures are still outstanding. Request changes records
// without completing.
//
// Plugin contract: window.__boss_register_step_plugin(kind, mount);
// mount(container, { step, jobId, onUpdate, currentUser }).

(function () {
  function h(tag, attrs, ...children) {
    const el = document.createElement(tag);
    if (attrs) {
      for (const k in attrs) {
        const v = attrs[k];
        if (v == null || v === false) continue;
        if (k === 'className') el.className = v;
        else if (k.startsWith('on') && typeof v === 'function') {
          el.addEventListener(k.slice(2).toLowerCase(), v);
        } else if (k === 'disabled' || k === 'value') {
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

  function when(iso) {
    if (!iso) return '';
    const d = new Date(iso);
    return Number.isNaN(d.getTime()) ? String(iso) : d.toLocaleString();
  }

  // The decision trio is the step KIND's own vocabulary (seeded with
  // the kind when it absorbed the retired `approval` kind); declared
  // per-workflow fields are anything beyond it.
  const TRIO = ['decision', 'decided_at', 'comment'];

  function nonEmptyString(v) {
    return typeof v === 'string' && v.trim().length > 0 ? v : null;
  }

  function mount(container, { step, jobId, onUpdate }) {
    const required = Array.isArray(step.sign_offs_required) ? step.sign_offs_required : [];
    let stamps = Array.isArray(step.sign_offs) ? step.sign_offs.slice() : [];
    const isDone = step.status === 'completed' || step.status === 'skipped';
    let busy = false;
    let error = null;

    const declared = (Array.isArray(step.fields) ? step.fields : []).filter(
      (f) => f && f.name && !TRIO.includes(f.name),
    );
    // Live values for declared fields, seeded from step metadata (a
    // pre-filled `approved` renders filled and editable).
    const fieldValues = {};
    declared.forEach((f) => {
      const cur = (step.metadata || {})[f.name];
      fieldValues[f.name] = cur == null ? '' : String(cur);
    });

    const stampFor = (role) => stamps.find((s) => s && s.role === role);
    const outstanding = () => required.filter((r) => !stampFor(r));
    const missingRequired = () =>
      declared.filter((f) => f.required && !nonEmptyString(fieldValues[f.name]));

    const contextDiv = h('div', { className: 'step-signoff-context' });
    const fieldsDiv = h('div', { className: 'step-signoff-fields' });
    const rolesDiv = h('div', { className: 'step-signoff-roles' });
    const actionsDiv = h('div', { className: 'step-actions' });
    const errorDiv = h('div', { className: 'step-signoff-error' });
    const commentTa = h('textarea', {
      className: 'step-signoff-comment',
      rows: '2',
      placeholder: 'Comment (optional)…',
    });
    commentTa.value = String((step.metadata || {}).comment || '');

    function renderContext(text, sourceLabel) {
      contextDiv.replaceChildren();
      if (!text) return;
      // The case renders as MARKDOWN when the host provides its
      // escape-first renderer (window.__boss_markdown, one definition
      // for every bundle — 2244db9e), and as preserved text when it
      // does not (older SPA, tests). The innerHTML is earned by the
      // renderer's contract: everything is escaped before any tag it
      // emits, hrefs are http(s)/relative only.
      const render = window.__boss_markdown;
      const body = h('div', { className: 'step-signoff-context-body' });
      if (typeof render === 'function') {
        body.innerHTML = render(text);
      } else {
        body.textContent = text;
      }
      contextDiv.appendChild(
        h(
          'div',
          { className: 'step-signoff-context-card' },
          h(
            'div',
            { className: 'step-signoff-context-head' },
            h('span', { className: 'step-signoff-context-title' }, 'What this decision is about'),
            h('span', { className: 'step-signoff-context-source' }, sourceLabel),
          ),
          body,
        ),
      );
    }

    function renderFields() {
      fieldsDiv.replaceChildren();
      if (declared.length === 0) return;
      declared.forEach((f) => {
        const id = `signoff-field-${step.id}-${f.name}`;
        const type = String(f.field_type || 'string');
        let input;
        if (type.includes('|')) {
          input = h('select', { className: 'step-signoff-input', id });
          const opts = type.split('|').map((o) => o.trim()).filter(Boolean);
          if (!opts.includes(fieldValues[f.name])) {
            input.appendChild(h('option', { value: '' }, '— choose —'));
          }
          opts.forEach((o) => input.appendChild(h('option', { value: o }, o)));
          input.value = fieldValues[f.name];
        } else {
          input = h('input', { className: 'step-signoff-input', id, type: 'text' });
          input.value = fieldValues[f.name];
        }
        input.addEventListener('input', (e) => {
          fieldValues[f.name] = e.target.value;
          renderActions();
        });
        input.addEventListener('change', (e) => {
          fieldValues[f.name] = e.target.value;
          renderActions();
        });
        if (isDone) input.disabled = true;
        fieldsDiv.appendChild(
          h(
            'div',
            { className: 'step-field' },
            h('label', { for: id }, f.required ? `${f.name} (required)` : f.name),
            input,
          ),
        );
      });
    }

    function renderRoles() {
      rolesDiv.replaceChildren();
      if (required.length === 0) {
        rolesDiv.appendChild(
          h(
            'p',
            { className: 'step-signoff-none' },
            'No counter-signatures are required — your decision completes the step.',
          ),
        );
        return;
      }
      required.forEach((role) => {
        const stamp = stampFor(role);
        const row = h(
          'div',
          { className: `step-signoff-role ${stamp ? 'is-signed' : 'is-outstanding'}` },
          h('span', { className: 'step-signoff-rolename' }, role),
          stamp
            ? h(
                'span',
                { className: 'step-signoff-stamp' },
                `signed by ${stamp.authority_id || 'unknown'} · ${when(stamp.stamped_at)}`,
              )
            : h('span', { className: 'step-signoff-await' }, 'awaiting signature'),
          !stamp && !isDone
            ? h(
                'button',
                { className: 'step-btn', disabled: busy, onClick: () => sign(role) },
                `Sign off as ${role}`,
              )
            : null,
        );
        rolesDiv.appendChild(row);
      });
    }

    function renderActions() {
      actionsDiv.replaceChildren();
      if (isDone) {
        const d = (step.metadata || {}).decision;
        if (d && d !== 'pending') {
          actionsDiv.appendChild(
            h('div', { className: `step-signoff-result step-signoff-${d}` }, `Decision: ${d}`),
          );
        }
        return;
      }
      const missing = missingRequired();
      if (missing.length > 0) {
        actionsDiv.appendChild(
          h(
            'span',
            { className: 'step-signoff-blocked' },
            `Fill the required field${missing.length === 1 ? '' : 's'} first: ${missing
              .map((f) => f.name)
              .join(', ')}`,
          ),
        );
      }
      const disabled = busy || missing.length > 0;
      actionsDiv.appendChild(
        h(
          'button',
          { className: 'step-btn step-btn-approve', disabled, onClick: () => decide('approved') },
          'Approve',
        ),
      );
      actionsDiv.appendChild(
        h(
          'button',
          { className: 'step-btn step-btn-reject', disabled, onClick: () => decide('rejected') },
          'Reject',
        ),
      );
      actionsDiv.appendChild(
        h(
          'button',
          {
            className: 'step-btn',
            disabled: busy,
            onClick: () => decide('changes-requested'),
          },
          'Request changes',
        ),
      );
    }

    function renderError() {
      errorDiv.replaceChildren();
      if (!error) return;
      errorDiv.appendChild(h('p', { className: 'step-error' }, error));
    }

    function renderAll() {
      renderFields();
      renderRoles();
      renderActions();
      renderError();
    }

    // Presence ceremony (docs/design/presence.md): a presence-gated
    // step refuses a plain stamp with 422 {required:"presence"}; the
    // passkey then signs a challenge bound to this step's CURRENT
    // shape hash and the stamp retries with the issued ticket. Plugins
    // are self-contained bundles, so the ceremony rides along here
    // rather than importing the app's helper. No fallback path (Q3).
    const b64uBytes = (s) => {
      const pad = s.length % 4 === 2 ? '==' : s.length % 4 === 3 ? '=' : '';
      return Uint8Array.from(atob(s.replace(/-/g, '+').replace(/_/g, '/') + pad), (c) =>
        c.charCodeAt(0),
      );
    };
    const bytesB64u = (buf) =>
      btoa(String.fromCharCode(...new Uint8Array(buf)))
        .replace(/\+/g, '-')
        .replace(/\//g, '_')
        .replace(/=+$/, '');
    async function presenceTicket() {
      const begin = await fetch('/api/auth/passkey/assert/begin', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ job_id: jobId, step_id: step.id }),
      });
      if (begin.status === 409) throw new Error('No passkey enrolled — add one first.');
      if (!begin.ok) throw new Error(`presence ceremony unavailable (${begin.status})`);
      const opts = await begin.json();
      const cred = await navigator.credentials.get({
        publicKey: {
          challenge: b64uBytes(opts.publicKey.challenge).buffer,
          rpId: opts.publicKey.rpId || undefined,
          allowCredentials: (opts.publicKey.allowCredentials || []).map((c) => ({
            type: c.type,
            id: b64uBytes(c.id).buffer,
          })),
          userVerification: opts.publicKey.userVerification,
          timeout: opts.publicKey.timeout,
        },
      });
      if (!cred) throw new Error('Passkey prompt returned no credential.');
      const a = cred.response;
      const finish = await fetch('/api/auth/passkey/assert/finish', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          challenge_id: opts.challenge_id,
          credential: {
            id: cred.id,
            rawId: bytesB64u(cred.rawId),
            type: cred.type,
            response: {
              authenticatorData: bytesB64u(a.authenticatorData),
              clientDataJSON: bytesB64u(a.clientDataJSON),
              signature: bytesB64u(a.signature),
              userHandle: a.userHandle ? bytesB64u(a.userHandle) : null,
            },
          },
        }),
      });
      if (!finish.ok) throw new Error(`assertion rejected (${finish.status})`);
      return (await finish.json()).ticket;
    }

    async function sign(role) {
      busy = true;
      error = null;
      renderAll();
      try {
        let res = await fetch(`/api/jobs/${jobId}/steps/${step.id}/sign-offs`, {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ role }),
        });
        if (res.status === 422) {
          const refusal = await res
            .clone()
            .json()
            .catch(() => null);
          if (refusal && refusal.required === 'presence') {
            const ticket = await presenceTicket();
            res = await fetch(`/api/jobs/${jobId}/steps/${step.id}/sign-offs`, {
              method: 'POST',
              headers: {
                'Content-Type': 'application/json',
                'x-presence-ticket': ticket,
              },
              body: JSON.stringify({ role }),
            });
          }
        }
        if (!res.ok) {
          error = `Could not record the ${role} signature (${res.status}). ${await res.text()}`;
          return false;
        } else {
          // Re-read rather than assume: the server decides attribution
          // and the shape hash the stamp pins.
          const fresh = await fetch(`/api/jobs/${jobId}`).then((r) => (r.ok ? r.json() : null));
          const s = fresh && (fresh.steps || []).find((x) => x.id === step.id);
          if (s) stamps = Array.isArray(s.sign_offs) ? s.sign_offs : [];
          if (typeof onUpdate === 'function') onUpdate();
          return true;
        }
      } catch (e) {
        error = `Could not record the ${role} signature: ${e}`;
        return false;
      } finally {
        busy = false;
        renderAll();
      }
    }

    async function decide(d) {
      busy = true;
      error = null;
      renderAll();
      try {
        // 1. The decision and the declared fields land in metadata
        //    FIRST — a stamp attests the step's shape, so the content
        //    being signed must already be in it. They travel through
        //    the step metadata PATCH, which merges ONLY the keys this
        //    surface owns against the row as it stands. The old idiom
        //    spread the page-load snapshot into a metadata PUT, which
        //    replaces wholesale — so any key another writer added
        //    after this page loaded was silently erased (the lost
        //    update that reverted a review's title/markdown on
        //    2026-09-02).
        const patch = {};
        declared.forEach((f) => {
          if (nonEmptyString(fieldValues[f.name])) patch[f.name] = fieldValues[f.name];
        });
        patch.decision = d;
        patch.decided_at = new Date().toISOString();
        if (commentTa.value.trim()) patch.comment = commentTa.value.trim();
        const saved = await fetch(`/api/jobs/${jobId}/steps/${step.id}/metadata`, {
          method: 'PATCH',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify(patch),
        });
        if (!saved.ok) {
          error = `Could not record the decision (${saved.status}): ${await saved.text()}`;
          return;
        }
        // Fold the merged keys into the local cache the same way the
        // server just did; keys other writers own stay as loaded.
        step.metadata = Object.assign(step.metadata || {}, patch);
        if (d === 'changes-requested') {
          if (typeof onUpdate === 'function') onUpdate();
          return;
        }
        // 2. The user's own signature, in the SAME gesture when exactly
        //    one role is outstanding — the single-signer case, which is
        //    most decisions. It runs AFTER the decision landed, because
        //    a stamp attests the step's shape (metadata included):
        //    on 2026-09-05 David signed first, this flow re-saved the
        //    decision with a fresh decided_at two seconds later, and the
        //    completion answered 409 stale — his own signature undone by
        //    the surface that asked for it. Nothing below writes
        //    metadata again. The server still enforces who may stamp;
        //    a refusal shows as the signature error and the decision
        //    stays recorded. (Per-role sign buttons remain for
        //    multi-party steps, where the ceremony is not this user's.)
        let left = outstanding();
        if (left.length === 1) {
          const signed = await sign(left[0]);
          if (!signed) return;
          left = outstanding();
        }
        // 3. Complete — unless other signatures are outstanding, in
        //    which case the decision is recorded and the roster says
        //    plainly what everyone is waiting on.
        if (left.length > 0) {
          error = null;
          renderAll();
          if (typeof onUpdate === 'function') onUpdate();
          return;
        }
        const done = await fetch(`/api/jobs/${jobId}/steps/${step.id}`, {
          method: 'PUT',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ status: 'completed' }),
        });
        if (!done.ok) {
          // 400: a required-at-done contract this surface did not
          // satisfy — name it, never swallow it (v1's ApprovalSurface
          // sibling swallowed these, which is how a click could
          // silently do nothing). 409: stale stamps; the server's own
          // text names which roles.
          error = `${done.status}: ${await done.text()}`;
          return;
        }
        if (typeof onUpdate === 'function') onUpdate();
      } catch (e) {
        error = `Could not record the decision: ${e}`;
      } finally {
        busy = false;
        renderAll();
      }
    }

    const root = h(
      'div',
      { className: 'step-signoff' },
      contextDiv,
      fieldsDiv,
      h('div', { className: 'step-signoff-head' }, 'Signatures'),
      rolesDiv,
      h('div', { className: 'step-field' }, commentTa),
      errorDiv,
      actionsDiv,
    );
    renderAll();
    container.appendChild(root);

    // The case for action, resolved the DecisionContext way: the
    // step's own context wins without a fetch; otherwise one job read
    // supplies the packet-level briefing or the filed message.
    const own = nonEmptyString((step.metadata || {}).context_md);
    if (own) {
      renderContext(own, 'written for this step');
    } else {
      fetch(`/api/jobs/${jobId}`)
        .then((r) => (r.ok ? r.json() : null))
        .then((job) => {
          const jm = (job && job.metadata) || {};
          const ctx = nonEmptyString(jm.context_md);
          if (ctx) return renderContext(ctx, 'the packet’s briefing');
          const msg = nonEmptyString(jm.message);
          if (msg) return renderContext(msg, 'the packet as filed');
        })
        .catch(() => {
          // No context is a quiet absence, never a broken surface.
        });
    }

    return () => root.remove();
  }

  if (typeof window.__boss_register_step_plugin !== 'function') {
    console.error('[sign-off-plugin] __boss_register_step_plugin not on window');
    return;
  }
  window.__boss_register_step_plugin('sign-off', mount);
})();
