// review-design.js — custom Step UX for the design-doc-review JobKind.
//
// Reads the questions the PACKET carries — `metadata.questions` on
// the step, or on the Job when an author put them there — and renders
// a per-question resolution textarea. Step completion is GATED on
// every question having a non-empty resolution recorded.
//
// Resolutions are saved onto the STEP, which IS the record. Two round
// trips through markdown files used to hang off this surface and both
// are gone (backlog f5da586c): the mirror into
// /api/design/pending-decisions that fed a flush job rewriting the
// source doc (deleted 2026-09-10, part 1), and the fallback fetch of
// /api/design/docs/{path} for a packet carrying only a pointer
// (deleted 2026-09-10, part 2, with the corpus index itself). The
// packet is the doc. Settled material folds into
// docs/architecture-decisions.md each release.
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

/* Exhibits — the packet's own HTML renderings (design 26a89f11), each in
   a sandboxed frame below the prose. The frame's box is resizable so a
   reviewer can give a prototype the height it wants. */
.step-review-design .srd-exhibits { margin-top: 28px; }
.step-review-design .srd-exhibits > h2 { margin-top: 0; }
.step-review-design .srd-exhibit { margin: 0 0 24px; scroll-margin-top: 12px; }
.step-review-design .srd-exhibit figcaption {
  display: flex; gap: 8px; align-items: baseline; flex-wrap: wrap;
  font-size: 13px; margin-bottom: 8px;
}
.step-review-design .srd-exhibit-title { font-weight: 600; }
.step-review-design .srd-exhibit-meta { color: var(--text-dim, #78716c); font-size: 12px; }
.step-review-design .srd-exhibit-frame {
  height: 440px; min-height: 160px; resize: vertical; overflow: hidden;
  border: 1px solid var(--border, #e7e5e4); border-radius: 6px; background: #fff;
}
.step-review-design .srd-exhibit-frame iframe {
  display: block; width: 100%; height: 100%; border: 0; background: #fff;
}
.step-review-design .srd-exhibit.is-flagged .srd-exhibit-frame {
  outline: 2px solid var(--accent, #2563eb); outline-offset: 2px;
}
.step-review-design .srd-q-exhibits {
  display: flex; gap: 6px; flex-wrap: wrap; align-items: center;
  margin: 10px 0 0; font-size: 12px; color: var(--text-dim, #78716c);
}
.step-review-design .srd-q-exhibits button {
  font: inherit; font-size: 12px; cursor: pointer; padding: 2px 8px;
  border-radius: 4px; border: 1px solid var(--border, #e7e5e4);
  background: var(--bg, #fafaf9); color: var(--text, #1c1917);
}
.step-review-design .srd-q-exhibits button:hover { border-color: var(--accent, #2563eb); }

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

  // ---------------------------------------------------------------
  // EXHIBITS — a proposal whose substance is visual rides inside the
  // packet (design 26a89f11, David 2026-09-23).
  //
  // An exhibit's `html` is UNTRUSTED: any actor may write step
  // metadata, and this surface runs in the reviewer's authenticated
  // session. So it is never parsed into this document — no innerHTML,
  // no renderMarkdown — and renders ONLY inside a frame that:
  //
  //   - is sandboxed with `allow-scripts` and nothing else. Without the
  //     same-origin token the frame's origin is opaque: its script cannot
  //     reach this page, its cookies, its storage or the API as the
  //     reviewer. No forms, popups or top navigation either — the sandbox
  //     withholds every capability it does not name.
  //   - is loaded by `srcdoc`, set as an attribute (never spliced into
  //     markup here), so no byte of the exhibit can close the frame.
  //   - carries a CSP of `default-src 'none'` with inline style and
  //     script only: no network, no fetched assets, no connections. An
  //     exhibit that needs live data is a page, not an exhibit.
  //
  // The two values are constants so the test that pins them reads the
  // same strings the frame is given.
  // ---------------------------------------------------------------
  const EXHIBIT_SANDBOX = 'allow-scripts';
  const EXHIBIT_CSP = "default-src 'none'; style-src 'unsafe-inline'; script-src 'unsafe-inline'";

  function exhibitSrcdoc(html) {
    // The policy is the FIRST thing in the head, before one byte of the
    // exhibit, so nothing the exhibit carries runs outside it. A policy
    // the exhibit adds of its own can only narrow this one.
    return (
      '<!doctype html><html><head><meta charset="utf-8">' +
      `<meta http-equiv="Content-Security-Policy" content="${EXHIBIT_CSP}">` +
      '</head><body>' +
      html +
      '</body></html>'
    );
  }

  function exhibitFrame(ex) {
    const frame = document.createElement('iframe');
    // The sandbox BEFORE the document: the flags apply to what loads.
    frame.setAttribute('sandbox', EXHIBIT_SANDBOX);
    frame.setAttribute('referrerpolicy', 'no-referrer');
    frame.setAttribute('title', `Exhibit ${ex.anchor}: ${ex.title}`);
    frame.setAttribute('srcdoc', exhibitSrcdoc(ex.html));
    return frame;
  }

  function utf8Bytes(s) {
    return typeof TextEncoder === 'function' ? new TextEncoder().encode(s).length : s.length;
  }

  // The exhibits a packet carries, as this surface reads them: inline
  // `html`, or — over the 256 KB inline bound — a `file_ref` into the
  // file store with the `sha256` and `size_bytes` the attaching verb
  // confirmed by read-back. An element carrying neither is KEPT and
  // listed as having nothing to render — never dropped (26a89f11: "list
  // anchor/title/size/hash, never drop").
  function readExhibits(raw) {
    if (!Array.isArray(raw)) return [];
    return raw
      .filter((e) => e && typeof e === 'object')
      .map((e, i) => ({
        anchor: String(e.anchor || `E${i + 1}`),
        title: String(e.title || ''),
        html: typeof e.html === 'string' ? e.html : null,
        fileRef: typeof e.file_ref === 'string' && e.file_ref.trim() ? e.file_ref.trim() : null,
        sha256: typeof e.sha256 === 'string' ? e.sha256.trim().toLowerCase() : null,
        sizeBytes: typeof e.size_bytes === 'number' ? e.size_bytes : null,
      }));
  }

  // THE FILE_REFS ARM (design 26a89f11): an exhibit over the inline
  // bound lives in the file store, and this surface fetches its bytes
  // AS THE REVIEWER — the same `/api/files/{id}` the attachments panel
  // downloads through — and then hands them, as text, to exactly the
  // frame an inline exhibit gets (`exhibitFrame`). Nothing else changes:
  // same sandbox, same policy, same srcdoc, and the bytes never touch
  // this document. There is one render path, and this only feeds it.
  //
  // CHECKED BEFORE SHOWN. The record names the bytes that were reviewed
  // by their sha256 and size, and the store can answer with other bytes
  // (a detached file, a store switched off answering 200 with an
  // `unconfigured` envelope). So a size or digest that does not match is
  // refused, naming both — the reviewer never sees a rendering the
  // record does not vouch for. Where the digest cannot be computed (no
  // WebCrypto outside a secure context) or was never recorded, the
  // exhibit renders and SAYS it was not checked; silence is the one
  // thing it may not do.
  async function sha256Hex(buf) {
    const subtle = typeof crypto !== 'undefined' && crypto && crypto.subtle;
    if (!subtle) return null;
    const digest = await subtle.digest('SHA-256', buf);
    return Array.from(new Uint8Array(digest))
      .map((b) => b.toString(16).padStart(2, '0'))
      .join('');
  }

  async function loadFileExhibit(ex) {
    let r;
    try {
      r = await fetch(`/api/files/${encodeURIComponent(ex.fileRef)}`, {
        credentials: 'same-origin',
      });
    } catch (e) {
      return { error: `file ${ex.fileRef} could not be fetched: ${e && e.message}` };
    }
    if (!r.ok) {
      return { error: `the file store answered HTTP ${r.status} for file ${ex.fileRef}` };
    }
    const buf = await r.arrayBuffer();
    if (ex.sizeBytes !== null && buf.byteLength !== ex.sizeBytes) {
      return {
        error:
          `file ${ex.fileRef} served ${buf.byteLength.toLocaleString()} bytes; the record ` +
          `says ${ex.sizeBytes.toLocaleString()} — not rendered`,
      };
    }
    const digest = await sha256Hex(buf);
    if (ex.sha256 && digest && digest !== ex.sha256) {
      return {
        error:
          `file ${ex.fileRef} served bytes with sha256 ${digest}, which does not match the ` +
          `recorded ${ex.sha256} — not rendered`,
      };
    }
    let html;
    try {
      html = new TextDecoder('utf-8', { fatal: true }).decode(buf);
    } catch (_) {
      return { error: `file ${ex.fileRef} is not UTF-8 text — an exhibit is an HTML document` };
    }
    const checked = !!(ex.sha256 && digest);
    return {
      html,
      bytes: buf.byteLength,
      note: checked
        ? `sha256 checked against the record`
        : ex.sha256
          ? 'sha256 NOT checked — this browser offers no digest outside a secure context'
          : 'sha256 NOT checked — the record carries none',
    };
  }

  // A question's bindings: the exhibit anchors its `exhibits` key names.
  function questionExhibits(q) {
    return Array.isArray(q && q.exhibits) ? q.exhibits.map(String) : [];
  }

  function mount(container, { step, jobId, onUpdate }) {
    const docPath = (step.metadata && step.metadata.doc_path) || '';
    // resolutions: [{ anchor, decision }] — anchor matches the
    // anchor the packet's question carries (e.g. "Q1", "Q2", ...).
    let resolutions = Array.isArray(step.metadata && step.metadata.resolutions)
      ? step.metadata.resolutions.map((r) => ({
          anchor: String(r.anchor || ''),
          decision: String(r.decision || ''),
        }))
      : [];

    let doc = null;
    let questions = [];
    let exhibits = [];
    // Each exhibit's rendered <figure>, by anchor, so a question's
    // binding can bring its exhibit into view beside it.
    const exhibitFigures = {};
    // A by-reference exhibit's fetch, by anchor — one per mount however
    // often the body re-renders.
    const fileExhibitLoads = {};
    // True when the questions came from the packet rather than the
    // docs API. Kept because the loader's four branches need to know
    // which of them answered; it no longer decides where answers go,
    // since every answer now lives only on the step.
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

    // THE UNSAVED ANSWER OUTLIVES THE MOUNT (backlog fec57f5f).
    //
    // An answer lives only in `resolutions` until Save, and this
    // closure dies with the mount. The host used to remount the plugin
    // on every packet reload, so a long answer David was typing kept
    // clearing itself — the plugin came back from the step's SAVED
    // resolutions, usually none. The host no longer does that, but a
    // genuine remount (a page reload, leaving and coming back, a
    // status move) still would. So every keystroke is also a draft in
    // sessionStorage, keyed by step and question, restored on mount
    // and spent by a successful save. Per-tab and best-effort: storage
    // can be absent or throw (a private window, blocked site data), and
    // then the surface behaves exactly as it did before.
    function draftKey(anchor) {
      return `boss.review-design.draft:${step.id}:${anchor}`;
    }
    function readDraft(anchor) {
      try {
        return window.sessionStorage.getItem(draftKey(anchor));
      } catch (_) {
        return null;
      }
    }
    function writeDraft(anchor, decision) {
      try {
        window.sessionStorage.setItem(draftKey(anchor), decision);
      } catch (_) {
        // No storage: the in-memory answer is all there is.
      }
    }
    function dropDraft(anchor) {
      try {
        window.sessionStorage.removeItem(draftKey(anchor));
      } catch (_) {
        // Nothing was stored.
      }
    }
    function upsertResolution(anchor, decision) {
      const idx = resolutions.findIndex((x) => x.anchor === anchor);
      if (idx >= 0) {
        resolutions[idx] = { anchor, decision };
      } else {
        resolutions.push({ anchor, decision });
      }
    }
    // A completed review is a record, not a form: its drafts are not
    // laid over what was decided.
    function restoreDrafts() {
      if (isDone) return;
      questions.forEach((q) => {
        const draft = readDraft(q.anchor);
        if (draft !== null && draft !== resolutionFor(q.anchor)) {
          upsertResolution(q.anchor, draft);
        }
      });
    }

    function setResolution(anchor, decision) {
      upsertResolution(anchor, decision);
      writeDraft(anchor, decision);
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
    /// `q.proposal` is a declared field of the packet's question
    /// metadata (`design-doc.toml`, `item_keys`).
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

    // The exhibits, in the READING pane below the prose — the wide pane,
    // beside the decision rail, so a bound exhibit sits next to the
    // question it is asked about when its card brings it into view. A
    // theme board or a prototype squeezed into the rail's width would
    // be reviewed at a size nobody will see it at.
    function renderExhibits() {
      if (!exhibits.length) return null;
      const section = h(
        'section',
        { className: 'srd-exhibits' },
        h('h2', null, `Exhibits (${exhibits.length})`),
      );
      exhibits.forEach((ex) => {
        const askedBy = questions
          .filter((q) => q.exhibits.includes(ex.anchor))
          .map((q) => q.anchor);
        const asked = askedBy.length ? ` · asked about in ${askedBy.join(', ')}` : '';
        const sandboxed = 'sandboxed: runs its own style and script, reaches nothing else';
        const inline = ex.html !== null;
        const meta = h(
          'span',
          { className: 'srd-exhibit-meta' },
          inline
            ? `${utf8Bytes(ex.html).toLocaleString()} bytes · ${sandboxed}${asked}`
            : ex.fileRef
              ? `file ${ex.fileRef} · loading from the file store…${asked}`
              : `carries neither inline html nor a file_ref — nothing to render${asked}`,
        );
        const frameBox = inline || ex.fileRef ? h('div', { className: 'srd-exhibit-frame' }) : null;
        if (inline) frameBox.appendChild(exhibitFrame(ex));
        const figure = h(
          'figure',
          { className: 'srd-exhibit' },
          h(
            'figcaption',
            null,
            h('span', { className: 'srd-anchor' }, ex.anchor),
            h('span', { className: 'srd-exhibit-title' }, ex.title),
            meta,
          ),
          frameBox,
        );
        // By reference: fetched once per mount, then handed to the SAME
        // frame an inline exhibit gets. A failure removes the frame box
        // and says what failed in its place.
        if (!inline && ex.fileRef) {
          if (!fileExhibitLoads[ex.anchor]) {
            fileExhibitLoads[ex.anchor] = loadFileExhibit(ex).catch((e) => ({
              error: `file ${ex.fileRef} could not be read: ${e && e.message}`,
            }));
          }
          fileExhibitLoads[ex.anchor].then((got) => {
            if (got.error) {
              frameBox.remove();
              meta.replaceChildren(document.createTextNode(`${got.error}${asked}`));
              return;
            }
            frameBox.appendChild(exhibitFrame({ ...ex, html: got.html }));
            meta.replaceChildren(
              document.createTextNode(
                `${got.bytes.toLocaleString()} bytes · file ${ex.fileRef} · ${got.note} · ` +
                  `${sandboxed}${asked}`,
              ),
            );
          });
        }
        exhibitFigures[ex.anchor] = figure;
        section.appendChild(figure);
      });
      return section;
    }

    // A bound question's link to its exhibits: one button per anchor,
    // bringing that exhibit into view in the reading pane and marking it.
    function questionExhibitLinks(q) {
      const bound = q.exhibits.filter((a) => exhibits.some((ex) => ex.anchor === a));
      if (!bound.length) return null;
      return h(
        'div',
        { className: 'srd-q-exhibits' },
        h('span', null, 'Exhibits:'),
        bound.map((anchor) => {
          const btn = h(
            'button',
            { type: 'button', title: `Show exhibit ${anchor} beside this question` },
            anchor,
          );
          btn.addEventListener('click', () => {
            const figure = exhibitFigures[anchor];
            if (!figure) return;
            Object.values(exhibitFigures).forEach((f) => f.classList.remove('is-flagged'));
            figure.classList.add('is-flagged');
            figure.scrollIntoView({ behavior: 'smooth', block: 'start' });
          });
          return btn;
        }),
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
      const shown = renderExhibits();
      if (shown) inner.appendChild(shown);
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
      restoreDrafts();
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
          questionExhibitLinks(q),
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

    async function mergeOwnedKeys() {
      // Merge ONLY the keys this surface owns, server-side. The old
      // idiom PUT `{ ...step.metadata, doc_path, resolutions }` — the
      // page-load snapshot plus our keys — and PUT metadata is
      // replaced WHOLESALE, so any key another writer added after this
      // page loaded (the carried title, the markdown) rode the stale
      // snapshot back out of existence: the lost update that reverted
      // a review's title/markdown and 400'd the reviewer twice on
      // 2026-09-02. The metadata PATCH merges top-level keys against
      // the row as it stands and preserves every key it does not name
      // (a null value would DELETE its key — never send one to keep).
      const r = await fetch(`/api/jobs/${jobId}/steps/${step.id}/metadata`, {
        method: 'PATCH',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ doc_path: docPath, resolutions }),
      });
      if (!r.ok) throw new Error(`step metadata merge HTTP ${r.status}: ${await r.text()}`);
    }

    async function freshStep() {
      // The metadata PATCH answers 204 with no body, so the post-merge
      // row must be read back before completing: the completion PUT
      // still replaces metadata wholesale, and completing with this
      // page's snapshot would re-introduce the exact lost update the
      // PATCH just avoided. There is no single-step GET; the job's
      // steps list is the read the API offers.
      const r = await fetch(`/api/jobs/${jobId}/steps`);
      if (!r.ok) throw new Error(`step read-back HTTP ${r.status}: ${await r.text()}`);
      const steps = await r.json();
      const fresh = Array.isArray(steps) ? steps.find((s) => s.id === step.id) : null;
      if (!fresh) throw new Error('step read-back: step missing from its own job');
      return fresh;
    }

    async function putStep(base, status) {
      // The step PUT overlays: any field the body omits keeps its
      // current value, so a status-only body (base = {}) moves the
      // status and touches nothing else — metadata included.
      const r = await fetch(`/api/jobs/${jobId}/steps/${step.id}`, {
        method: 'PUT',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ ...base, job_id: jobId, status }),
      });
      if (!r.ok) throw new Error(`step save HTTP ${r.status}: ${await r.text()}`);
    }

    async function save(autoComplete) {
      saving = true;
      saveError = null;
      renderActions();
      try {
        const completing = autoComplete && (allAnswered() || questions.length === 0);

        // 1. Land ALL metadata writes first (title + metadata are what
        //    sign-off stamps attest — a stamp taken before the last
        //    metadata write goes stale and the completion 409s).
        //    The answers are on the step now, so their drafts are spent
        //    — but only a draft still equal to what was SENT: text typed
        //    while the save was in flight is unsaved and keeps its draft.
        const sent = resolutions.map((r) => ({ ...r }));
        await mergeOwnedKeys();
        sent.forEach((r) => {
          if (readDraft(r.anchor) === r.decision) dropDraft(r.anchor);
        });

        // 2. A save has always flipped a pending step active before
        //    any stamp lands. Status cannot travel through the
        //    metadata PATCH, so it rides a status-only PUT.
        if (step.status === 'pending') await putStep({}, 'active');

        if (completing) {
          // 3. Read the post-merge row back — the stamps and the
          //    completion must attest the step's true final shape,
          //    not this page's snapshot.
          const fresh = await freshStep();
          // 4. Stamp every required sign-off role in the step's now-
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
          // 5. Complete with the identical metadata the stamps attest
          //    — the fresh row verbatim.
          await putStep(fresh, 'completed');
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
          exhibits: questionExhibits(q),
        }));
        // Exhibits ride the STEP — `boss design --exhibit` mirrors them
        // there with the questions, and completing the review freezes
        // them with the rest of the record.
        exhibits = readExhibits(step.metadata.exhibits);
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
        exhibits = readExhibits(step.metadata && step.metadata.exhibits);
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
                exhibits: questionExhibits(q),
              }));
              exhibits = readExhibits(jm.exhibits);
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
      // A POINTER-ONLY PACKET IS NO LONGER READABLE, and says so.
      //
      // Until 2026-09-10 this branch fetched `/api/design/docs/{path}`
      // — the corpus index that parsed `### Qn:` headings out of the
      // file on deployed main. That whole read half was deleted with
      // the rest of the markdown-corpus machinery (backlog f5da586c):
      // the packet is the doc, so a packet that carries only a pointer
      // carries nothing to review. Every review spawned since
      // 2026-08-18 carries its questions, and the three branches above
      // read both metadata bags.
      //
      // Named honestly rather than left as a fetch that 404s. The old
      // message apologised that "docs ride trains" and told the reader
      // to wait for a landing; waiting no longer helps, and a message
      // that sends someone to wait for something that will not happen
      // is worse than one that says what to do instead.
      loadError =
        `this packet carries only a pointer (metadata.doc_path = ` +
        `${docPath}) and no questions or prose of its own. The design ` +
        `corpus index that used to read the file was deleted on ` +
        `2026-09-10 — the packet is the doc now. Read ${docPath} in ` +
        `the repo, and file a fresh design-doc packet (\`boss design\`) ` +
        `carrying its questions to review it here.`;
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
