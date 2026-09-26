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
//   5. WHAT THE PASSKEY SIGNS — on a presence step, the title and EVERY
//      metadata key, drawn from the step's own keys (design f623e425
//      D3; backlog 6c9183de extends b and c, 2026-09-25). The passkey
//      binds step_shape_hash(title, metadata), and this surface drew
//      only the declared fields, so an ops-request's verb, host, args,
//      rendered_plan_sha256 or a planted decision was signed by the
//      per-role button unseen. A ceremony on a step whose block is not
//      drawn — a kind whose floor demands presence the step never
//      declared — signs nothing: it draws the block and asks for the
//      tap again. Title, key names and values are drawn AS THEIR BYTES —
//      quoted and escaped wherever a reader could otherwise misread them
//      (backlog 6093cf13) — a box that scrolls says so, and an unmounted
//      surface signs nothing.
//
// Order on Approve/Reject (v3, feedback 221b4b5c): metadata lands
// first (a stamp attests the step's current shape, so the decision
// must be IN the shape), then the user's own stamp if their role is
// required and unsigned, then the completion — skipped, with a plain
// explanation, while other roles' signatures are still outstanding.
// Request changes records without completing — unless the step's
// protocol declares `changes_requested_completes = true`, when it takes
// the Approve path (backlog da322e8f). NOTHING writes metadata
// after a signature exists: on 2026-09-05 15:40 David signed, then
// this surface re-saved his unchanged decision with a fresh
// decided_at, and the completion answered 409 stale two seconds after
// his signature. A decision that a signature already covers is not
// re-saved; one that changes is saved BEFORE the (re-)signature, and
// the roster says which stamps that made stale. Each stage of the
// gesture is shown as it lands — saved, signed, completed — and any
// refusal verbatim, so a tap never looks like it did nothing.
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

  // A signed value AS THE BYTES IT IS (backlog 6093cf13, adversarial
  // review of car 30674304): a string that reads as exactly itself is
  // drawn bare; any other — empty, spaced at an edge, multi-line, holding
  // a character that is not printable ASCII or an em dash, starting with
  // a quote, or reading as a number, boolean, null or JSON — is drawn in
  // double quotes with each such character written as \u{XXXX}. Anything
  // else is its indented JSON with the same escapes. So '42' and 42, a
  // bidi override, a zero-width space and a Cyrillic look-alike are all
  // visibly what they are. Key names and the title are drawn through it
  // too. The app's copy is signedText in apps/web/src/steps/presence.ts
  // (the reasoning is there); signOffPlugin.test.ts pins the two equal on
  // generated inputs, because a bundle cannot import it.
  const AS_ITSELF = /^[\x20-\x7E\u2014]$/u;
  const NOT_AS_ITSELF = /[^\x20-\x7E\n\u2014]/gu;
  const escaped = (c) =>
    `\\u{${(c.codePointAt(0) || 0).toString(16).toUpperCase().padStart(4, '0')}}`;
  const QUOTED = { '\\': '\\\\', '"': '\\"', '\n': '\\n\n', '\t': '\\t', '\r': '\\r' };
  const quoted = (s) =>
    `"${[...s]
      .map((c) =>
        Object.prototype.hasOwnProperty.call(QUOTED, c)
          ? QUOTED[c]
          : AS_ITSELF.test(c)
            ? c
            : escaped(c),
      )
      .join('')}"`;

  function readsAsItself(s) {
    if (s === '' || s.startsWith('"') || s.startsWith(' ') || s.endsWith(' ')) return false;
    if (![...s].every((c) => AS_ITSELF.test(c))) return false;
    try {
      JSON.parse(s);
      return false;
    } catch (_) {
      return true;
    }
  }

  function signedText(v) {
    if (typeof v === 'string') return readsAsItself(v) ? v : quoted(v);
    const s = JSON.stringify(v, null, 2);
    return s === undefined ? String(v) : s.replace(NOT_AS_ITSELF, escaped);
  }

  // What a value whose box scrolls says under it (6093cf13): rendered is
  // not read. The app's copy is scrollNote in presence.ts, pinned equal.
  function scrollNote(text) {
    const lines = text.split('\n').length;
    return `scrolls in its box: ${lines} ${lines === 1 ? 'line' : 'lines'}, ${[...text].length} characters. Read it to the end; your passkey signs all of it.`;
  }

  function canonical(v) {
    if (Array.isArray(v)) return `[${v.map(canonical).join(',')}]`;
    if (v !== null && typeof v === 'object') {
      return `{${Object.keys(v)
        .sort()
        .map((k) => `${JSON.stringify(k)}:${canonical(v[k])}`)
        .join(',')}}`;
    }
    const s = JSON.stringify(v);
    return s === undefined ? 'null' : s;
  }

  // What a passkey would sign in `shown` that `screen` (the step as this
  // surface last drew it, or null when it drew no signed content) does
  // not show as signed: 'title', then each metadata key missing or drawn
  // with another value. The app's copy is notShown in presence.ts, pinned
  // equal by signOffPlugin.test.ts.
  function notShown(shown, screen) {
    const keys = Object.keys(shown.metadata).sort();
    if (!screen) return ['title', ...keys];
    const out = shown.title === screen.title ? [] : ['title'];
    keys.forEach((k) => {
      if (
        !Object.prototype.hasOwnProperty.call(screen.metadata, k) ||
        canonical(screen.metadata[k]) !== canonical(shown.metadata[k])
      ) {
        out.push(k);
      }
    });
    return out;
  }

  function mount(container, { step, jobId, onUpdate }) {
    const required = Array.isArray(step.sign_offs_required) ? step.sign_offs_required : [];
    let stamps = Array.isArray(step.sign_offs) ? step.sign_offs.slice() : [];
    const isDone = step.status === 'completed' || step.status === 'skipped';
    let busy = false;
    let error = null;
    // Roles whose recorded stamp no longer matches the step's shape —
    // the server's rule (a stamp pins step_shape_hash; a completion-
    // relevant write after it records STEP_STAMPS_INVALIDATED and the
    // stamp stays for provenance). Tracked here so the roster tells
    // the truth between a change and the next read of the step.
    const stale = new Set();
    // The stages of the current gesture, in the order they landed.
    let progress = [];
    // The presence ticket the gateway issued for THIS surface's own
    // signature — kept so the completion carries it (backlog b568044a,
    // 2026-09-25). The jobs API judges assurance on every request that
    // leaves the open states, from that request's own header, so a
    // presence stamp followed by a bare completion PUT answered 422 and
    // the step stayed ready after the stamp. It is only ever a ticket a
    // ceremony on this step minted for this user, and it authorises
    // nothing new: the server re-checks its step, person, shape and
    // two-minute expiry on the PUT exactly as on the stamp, and a step
    // edited since the ceremony refuses it. Nothing here mints one.
    let presenceTicketHeld = null;
    // What the passkey signs, as last drawn: {title, metadata} copied at
    // the render, so a later write to the local cache is not mistaken for
    // what is on screen. null while no signed content is drawn.
    let onScreen = null;
    // Set when a ceremony was asked of a step that never declared
    // presence: from then on its signed content is drawn too.
    let revealed = false;
    // Set by the mount's cleanup (backlog 6093cf13). The host unmounts
    // this surface when the rail moves to another step, and a gesture
    // already running kept going: it drew into the detached tree, its
    // copy of "what is on screen" still matched, and the passkey prompt
    // came up over the NEXT step to sign this one. Unmounted, nothing of
    // this step is on screen, so nothing is drawn as signed and any
    // ceremony still in flight refuses.
    let disposed = false;
    // Aborted by the same cleanup, so a passkey prompt still up when the
    // rail moves on comes down with the surface rather than waiting over
    // the next step (backlog 7c53b1bf, review of car fcda5f8b).
    const unmounted = new AbortController();
    const signedVisible = () =>
      !disposed && !isDone && (step.assurance_required === 'presence' || revealed);
    // The overflow checks of the rows as last drawn, and the observers
    // that re-run them when a box changes size.
    let overflowChecks = [];
    let overflowObservers = [];

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
    // WHAT A PASSKEY SIGNS IS SHOWN, NOT OFFERED FOR EDIT (adversarial
    // re-review of fd7090cc, 2026-09-25). On a presence-assured step a
    // declared field that already holds a value is the document the
    // signature binds — an ops-request's `plan`, rendered on the host.
    // As a one-line text input it lost its newlines on screen, so the
    // approver read a flattened plan, and could edit it under the
    // signature. It renders read-only in a <pre>, byte for byte, and the
    // decision patch never re-writes it: this surface did not author it.
    const signedDoc = new Set(
      step.assurance_required === 'presence'
        ? declared.filter((f) => nonEmptyString(fieldValues[f.name])).map((f) => f.name)
        : [],
    );

    // Whether Request changes COMPLETES this step, read off the step's
    // own metadata — the protocol's declaration, not this surface's
    // guess (backlog da322e8f, 2026-09-23). An ordinary sign-off keeps
    // its step open on changes-requested so the same approver can
    // re-decide once the thing is revised. A protocol that ROUTES the
    // decision — page-audit's `revise` is ready_when `steps.review.done
    // AND decision = "changes-requested"` — needs the step done, and
    // declares it with `changes_requested_completes = true` in the
    // step's metadata_defaults. Undeclared, the old behaviour stands:
    // three founder change requests sat recorded and unrouted until the
    // operator completed them by hand, which is what this ends.
    const changesRequestedCompletes = (step.metadata || {}).changes_requested_completes === true;

    const stampFor = (role) => stamps.find((s) => s && s.role === role);
    const outstanding = () => required.filter((r) => !stampFor(r) || stale.has(r));
    const missingRequired = () =>
      declared.filter((f) => f.required && !nonEmptyString(fieldValues[f.name]));

    const contextDiv = h('div', { className: 'step-signoff-context' });
    const fieldsDiv = h('div', { className: 'step-signoff-fields' });
    const signedDiv = h('div', { className: 'step-signed-keys' });
    const rolesDiv = h('div', { className: 'step-signoff-roles' });
    const actionsDiv = h('div', { className: 'step-actions' });
    const errorDiv = h('div', { className: 'step-signoff-error' });
    const progressDiv = h('div', { className: 'step-signoff-progress' });
    const commentTa = h('textarea', {
      className: 'step-signoff-comment',
      rows: '2',
      placeholder: 'Comment (optional)…',
    });
    commentTa.value = String((step.metadata || {}).comment || '');

    // `signed` is false for the packet's own text (its briefing or filed
    // message): that is job metadata, outside the step's shape hash, so a
    // passkey on this step does not sign it and it can change under a
    // signature without voiding it — and its links are drawn as their
    // text, not their targets. The card says so (6093cf13). The step's
    // own context_md IS one of the step's keys, drawn as its bytes in the
    // signed block, and carries no such label.
    function renderContext(text, sourceLabel, signed) {
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
            signed
              ? null
              : h('span', { className: 'step-signoff-context-unsigned' }, 'not signed'),
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
        // Drawn once: while the signed block is up it carries this field,
        // byte for byte, with every other key the passkey signs.
        if (signedDoc.has(f.name) && signedVisible()) return;
        if (signedDoc.has(f.name)) {
          fieldsDiv.appendChild(
            h(
              'div',
              { className: 'step-field' },
              h('label', { for: id }, `${f.name} — what your passkey signs`),
              h('pre', { className: 'step-signoff-signed', id }, signedText(fieldValues[f.name])),
            ),
          );
          return;
        }
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

    // A value box that scrolls — its content taller or wider than the box
    // — carries a note under it saying how much there is (6093cf13). Run
    // at each draw, again once the root is attached (a detached box has no
    // size), and whenever a box changes size where the browser can say so.
    function watchOverflow(pre, note, text) {
      const check = () => {
        const scrolls =
          pre.scrollHeight > pre.clientHeight + 1 || pre.scrollWidth > pre.clientWidth + 1;
        note.textContent = scrolls ? scrollNote(text) : '';
      };
      overflowChecks.push(check);
      if (typeof ResizeObserver === 'function') {
        const observer = new ResizeObserver(check);
        observer.observe(pre);
        overflowObservers.push(observer);
      }
      check();
    }

    // Every key the passkey signs, from the step's own keys — never an
    // allow-list, so a key nobody wrote a renderer for is drawn as its
    // JSON rather than skipped — and the copy presenceTicket() compares
    // against is taken HERE, from what was just drawn. The title, each
    // key name and each value are drawn as their bytes (signedText).
    function renderSigned() {
      signedDiv.replaceChildren();
      overflowObservers.forEach((o) => o.disconnect());
      overflowObservers = [];
      overflowChecks = [];
      onScreen = null;
      if (!signedVisible()) return;
      const md = step.metadata || {};
      signedDiv.appendChild(
        h('div', { className: 'step-signed-keys-head' }, 'What your passkey signs'),
      );
      signedDiv.appendChild(
        h(
          'p',
          { className: 'step-signed-keys-note' },
          'The step ',
          h('strong', { className: 'step-signed-title' }, signedText(step.title)),
          ' and every key below, exactly as shown. Text in double quotes has each character you could not otherwise see or tell apart written as an escape. Your decision, its time and your comment join them when you press a button.',
        ),
      );
      Object.keys(md)
        .sort()
        .forEach((k) => {
          const text = signedText(md[k]);
          const pre = h('pre', { className: 'step-signed-value' }, text);
          const note = h('div', { className: 'step-signed-overflow' });
          signedDiv.appendChild(
            h(
              'div',
              { className: 'step-signed-row' },
              h('div', { className: 'step-signed-key' }, signedText(k)),
              pre,
              note,
            ),
          );
          watchOverflow(pre, note, text);
        });
      onScreen = { title: step.title, metadata: JSON.parse(JSON.stringify(md)) };
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
        const isStale = Boolean(stamp) && stale.has(role);
        const row = h(
          'div',
          {
            className: `step-signoff-role ${stamp && !isStale ? 'is-signed' : 'is-outstanding'}`,
          },
          h('span', { className: 'step-signoff-rolename' }, role),
          stamp
            ? h(
                'span',
                { className: isStale ? 'step-signoff-stale' : 'step-signoff-stamp' },
                `signed by ${stamp.authority_id || 'unknown'} · ${when(stamp.stamped_at)}${
                  isStale ? ' — stale: the step changed after signing; sign again' : ''
                }`,
              )
            : h('span', { className: 'step-signoff-await' }, 'awaiting signature'),
          (!stamp || isStale) && !isDone
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
            // A Request changes that completes meets the same
            // required-at-done contract as Approve, so it waits too.
            disabled: changesRequestedCompletes ? disabled : busy,
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

    function renderProgress() {
      progressDiv.replaceChildren();
      if (progress.length === 0) return;
      progressDiv.appendChild(
        h('p', { className: 'step-signoff-stages' }, progress.join(' · ')),
      );
    }

    function renderAll() {
      renderFields();
      renderSigned();
      renderRoles();
      renderActions();
      renderProgress();
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
    //
    // THE BEGIN NAMES WHAT THIS SURFACE SHOWED (backlog fd7090cc, the
    // security re-review of 2026-09-25): the step as rendered, with this
    // gesture's own decision folded in by decide() below — never a fresh
    // read. The gateway hashes it and refuses (412) a begin whose shown
    // step is not the step as it stands, so a plan swapped between the
    // render and the key press is never what the passkey signs.
    //
    // This is the ONE place the mount-prop snapshot rightly leaves the
    // page, and it is not a write: assert/begin stores nothing on the
    // step, it only compares. The lost update step-plugins-own-their-keys
    // refuses cannot happen here — a stale snapshot is refused 412, which
    // is the whole point — so the snapshot is named for what it is.
    // Unmounted mid-gesture (6093cf13): the step this gesture began on is
    // no longer on screen, so the passkey is never asked, and an answer it
    // already gave is never sent to be turned into a ticket.
    const offScreen = () =>
      new Error('Nothing was signed: this step is no longer on screen, so your passkey was not used for it.');

    async function presenceTicket() {
      if (disposed) throw offScreen();
      const renderedMetadata = step.metadata || {};
      // THE PASSKEY SIGNS ONLY WHAT WAS DRAWN (design f623e425 D3): a key
      // of what the begin would name that the signed block did not draw
      // as it would be signed refuses here, before any request. The only
      // way this surface reaches it is a step that never declared
      // presence: the block is drawn now and the tap asked for again, so
      // the approver reads before signing.
      const unseen = notShown({ title: step.title, metadata: renderedMetadata }, onScreen);
      if (unseen.length > 0) {
        revealed = true;
        renderAll();
        throw new Error(
          `nothing was signed: your passkey would sign ${unseen.join(', ')}, which this page had not shown — it is shown now; read it and press again`,
        );
      }
      const begin = await fetch('/api/auth/passkey/assert/begin', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          job_id: jobId,
          step_id: step.id,
          shown: { title: step.title, metadata: renderedMetadata },
        }),
      });
      if (begin.status === 409) throw new Error('No passkey enrolled — add one first.');
      if (!begin.ok) {
        // The gateway's refusal text names which of its steps refused
        // (job fetch, stored passkeys, challenge mint); the status alone
        // does not. The app's own copy of the ceremony says the same
        // since 2e893e27 (backlog f3436d99).
        const text = await begin.text().catch(() => '');
        const refused = new Error(`presence ceremony unavailable (${begin.status}): ${text}`);
        // The status travels with the error: a 412 means the step moved
        // under this surface, which its callers answer differently.
        refused.status = begin.status;
        throw refused;
      }
      const opts = await begin.json();
      if (disposed) throw offScreen();
      let cred;
      try {
        cred = await navigator.credentials.get({
          signal: unmounted.signal,
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
      } catch (err) {
        // A prompt the cleanup aborted is the refusal it is, not the
        // browser's AbortError.
        if (disposed) throw offScreen();
        throw err;
      }
      if (!cred) throw new Error('Passkey prompt returned no credential.');
      if (disposed) throw offScreen();
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
      if (!finish.ok) {
        // e.g. 410 'challenge already spent or expired — begin again',
        // or the verifier's own reason on a 401.
        const text = await finish.text().catch(() => '');
        throw new Error(`assertion rejected (${finish.status}): ${text}`);
      }
      const { ticket } = await finish.json();
      // Unmounted while the finish was in flight: the ticket is never
      // stamped with, so no stamp lands for a step no longer on screen;
      // unspent, it lapses in its two minutes (7c53b1bf).
      if (disposed) throw offScreen();
      return ticket;
    }

    // A begin refused 412 says the step no longer matches what this
    // surface shows — another writer moved it (backlog d82b5f60, review of
    // car 66de0e4b). What this mount rendered, "Decision saved" included,
    // is no longer what the step holds, and nothing was signed. So the
    // stale claim comes down, the host is asked to refresh, and the
    // approver is told to reopen the step rather than sign a copy this
    // mount can no longer vouch for.
    function stepMovedUnderUs() {
      progress = ['The step changed since it was shown, so nothing was signed — reopen it to read it as it stands'];
      if (typeof onUpdate === 'function') onUpdate();
    }

    async function sign(role) {
      const wasBusy = busy;
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
            if (res.ok) presenceTicketHeld = ticket;
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
          stale.delete(role);
          progress.push(`Signed as ${role}`);
          if (typeof onUpdate === 'function') onUpdate();
          return true;
        }
      } catch (e) {
        error = `Could not record the ${role} signature: ${e}`;
        if (e && e.status === 412) stepMovedUnderUs();
        return false;
      } finally {
        busy = wasBusy;
        renderAll();
      }
    }

    async function decide(d) {
      busy = true;
      error = null;
      progress = [];
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
          if (signedDoc.has(f.name)) return;
          if (nonEmptyString(fieldValues[f.name])) patch[f.name] = fieldValues[f.name];
        });
        patch.decision = d;
        if (commentTa.value.trim()) patch.comment = commentTa.value.trim();
        // A signature already on the step covers exactly what is in
        // it. When this gesture would change none of that, it is NOT
        // re-saved — a fresh decided_at alone would move the shape the
        // stamp pins and make the stamp stale (2026-09-05, 15:40:22).
        // The decided_at that stands is the one that was signed.
        const current = step.metadata || {};
        const unchanged = Object.keys(patch).every((k) => current[k] === patch[k]);
        if (stamps.length > 0 && unchanged) {
          progress.push(`Decision already recorded: ${d}`);
        } else {
          patch.decided_at = new Date().toISOString();
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
          progress.push(`Decision saved: ${d}`);
          // Every stamp attested the shape before this write; the
          // server has just marked them stale, and so does the roster.
          stamps.forEach((st) => st && stale.add(st.role));
        }
        if (d === 'changes-requested' && !changesRequestedCompletes) {
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
          progress.push(`Waiting on: ${left.join(', ')}`);
          renderAll();
          if (typeof onUpdate === 'function') onUpdate();
          return;
        }
        // The completion carries the ticket this surface's signature was
        // granted on, when it holds one: a presence-gated step is judged
        // again on this request, and the stamp does not lend it its
        // assurance (b568044a).
        const complete = (ticket) => {
          const headers = { 'Content-Type': 'application/json' };
          if (ticket) headers['x-presence-ticket'] = ticket;
          return fetch(`/api/jobs/${jobId}/steps/${step.id}`, {
            method: 'PUT',
            headers,
            body: JSON.stringify({ status: 'completed' }),
          });
        };
        // The held ticket is spent on the attempt it rode, whatever the
        // answer: kept, it rode every later completion from this mount
        // long past its two-minute life (backlog 3ce3c15f). In a finally,
        // because a request that THREW has no answer, and the ticket
        // outlived it (d82b5f60).
        let done;
        try {
          done = await complete(presenceTicketHeld);
        } finally {
          presenceTicketHeld = null;
        }
        // A completion refused for PRESENCE — after a reload the stamp is
        // already on the step and this mount holds no ticket; a held one
        // may have expired; or no role this user signs is required — is
        // answered with ONE ceremony on the step as shown, and ONE retry
        // (3ce3c15f). It used to print the raw 422, and the only way on
        // was to edit the comment until the shape moved and a signature
        // was forced. The ticket is the gateway's, minted by that
        // ceremony for this step and this person; the server judges it on
        // the retry exactly as on a stamp. Never a second ceremony.
        const refusedForPresence = async (res) => {
          if (res.status !== 422) return false;
          const refusal = await res
            .clone()
            .json()
            .catch(() => null);
          return Boolean(refusal && refusal.required === 'presence');
        };
        let retried = false;
        if (await refusedForPresence(done)) {
          progress.push('Completing needs your passkey');
          renderAll();
          let ticket;
          try {
            ticket = await presenceTicket();
          } catch (e) {
            error = `Could not complete: ${e && e.message ? e.message : e}`;
            // The decision DID land: the host re-reads the step, and a
            // 412 also takes down the claim this mount can no longer
            // vouch for (d82b5f60).
            if (e && e.status === 412) stepMovedUnderUs();
            else if (typeof onUpdate === 'function') onUpdate();
            return;
          }
          done = await complete(ticket);
          retried = true;
        }
        if (!done.ok) {
          // 400: a required-at-done contract this surface did not
          // satisfy — name it, never swallow it (v1's ApprovalSurface
          // sibling swallowed these, which is how a click could
          // silently do nothing). 409: stale stamps; the server's own
          // text names which roles. Only a retry refused for PRESENCE
          // again is "refused again after a fresh passkey tap" — a 409
          // after the tap is labelled by its own reason (d82b5f60).
          const again = retried && (await refusedForPresence(done));
          const text = await done.text();
          error = again
            ? `The completion was refused again after a fresh passkey tap — ${done.status}: ${text}`
            : `${done.status}: ${text}`;
          // The 409 names the roles whose stamps the server will not
          // accept; the roster offers those signatures again rather
          // than showing them as signed.
          try {
            const body = JSON.parse(text);
            (Array.isArray(body.missing_or_stale_roles) ? body.missing_or_stale_roles : []).forEach(
              (r) => stale.add(r),
            );
          } catch (_) {
            // Not JSON: the text itself is the whole explanation.
          }
          return;
        }
        progress.push('Completed');
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
      signedDiv,
      h('div', { className: 'step-signoff-head' }, 'Signatures'),
      rolesDiv,
      h('div', { className: 'step-field' }, commentTa),
      progressDiv,
      errorDiv,
      actionsDiv,
    );
    renderAll();
    container.appendChild(root);
    overflowChecks.forEach((check) => check());

    // The case for action, resolved the DecisionContext way: the
    // step's own context wins without a fetch; otherwise one job read
    // supplies the packet-level briefing or the filed message.
    const own = nonEmptyString((step.metadata || {}).context_md);
    if (own) {
      renderContext(own, 'written for this step', true);
    } else {
      fetch(`/api/jobs/${jobId}`)
        .then((r) => (r.ok ? r.json() : null))
        .then((job) => {
          const jm = (job && job.metadata) || {};
          const ctx = nonEmptyString(jm.context_md);
          if (ctx) return renderContext(ctx, 'the packet’s briefing', false);
          const msg = nonEmptyString(jm.message);
          if (msg) return renderContext(msg, 'the packet as filed', false);
        })
        .catch(() => {
          // No context is a quiet absence, never a broken surface.
        });
    }

    return () => {
      disposed = true;
      unmounted.abort();
      overflowObservers.forEach((o) => o.disconnect());
      overflowObservers = [];
      root.remove();
    };
  }

  // The copies of presence.ts's functions, where signOffPlugin.test.ts
  // can hold them equal to the app's on generated inputs (CLAUDE.md §9a;
  // 6093cf13 — the pin used to reach signedText alone, and canonical and
  // notShown only through one empty-screen case). Pure functions of their
  // arguments: exposing them grants nothing.
  mount.signed = Object.freeze({ signedText, canonical, notShown, scrollNote });

  if (typeof window.__boss_register_step_plugin !== 'function') {
    console.error('[sign-off-plugin] __boss_register_step_plugin not on window');
    return;
  }
  window.__boss_register_step_plugin('sign-off', mount);
})();
