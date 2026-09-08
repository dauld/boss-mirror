// answer-question.js — the question-and-response decision surface.
//
// v2 (0ab5fa3a, David accepted 2026-08-19): reads the step's OWN
// metadata first, the Job's second. v1 read only the Job, which was
// right for the approval Workflow (one decide step per packet) and
// wrong for every protocol that carries the brief on the step —
// user-feedback v11's design-review steps hold their docket brief in
// step.metadata.context_md, where the agent files it. The question
// falls back to the Job's filed message: on a feedback packet, what
// the filer wrote IS the question being decided.
//
// David, 2026-08-14, on the first version of this protocol: "I don't
// have any context for the question. The sender should be able to
// supply info, probably a markdown panel, that they want me to see
// when I answer the question. This will also provide the canvas to
// provide evidence or the artifact for an approval version of this
// general question, which is 'does this meet your bar?' essentially."
//
// So the surface is two halves, the same shape the design review
// settled on: the thing you must READ on the left, the decision you
// must RECORD on the right. The left is markdown the asker wrote into
// `job.metadata.context_md` — evidence, a diff, a measurement, the
// artifact itself. Without it an approval is a request to guess.
//
// The proposed answer is offered, not applied: `job.metadata.proposed`
// renders with a control that copies it into the answer box, and the
// box starts empty. Same contract the review rail uses (David: "you
// populate the resolution but I have to still click the button ...
// authorizing you to copy and paste on my behalf").
//
// Plugin contract: window.__boss_register_step_plugin(kind, mount).
// Host calls mount(container, props) with { step, jobId, onUpdate }.

(function () {
  const STYLE_ID = 'boss-answer-question-styles';
  const STYLES = `
.step-answer-question { --aq-gap: 20px; }

.step-answer-question .aq-head {
  display: flex; align-items: baseline; gap: 12px; flex-wrap: wrap;
  padding-bottom: 12px; margin-bottom: var(--aq-gap);
  border-bottom: 1px solid var(--border, #e7e5e4);
}
.step-answer-question .aq-head h3 { margin: 0; font-size: 17px; flex: 1 1 auto; line-height: 1.35; }
.step-answer-question .aq-asker {
  font-size: 12px; color: var(--text-dim, #78716c); white-space: nowrap;
}

/* Two panes. Same 1fr/1fr split with a reading floor the review
   surface uses — the answer box is where the work happens, so it gets
   real width rather than a sliver. */
.step-answer-question .aq-panes {
  display: grid; grid-template-columns: minmax(46ch, 1fr) minmax(320px, 1fr);
  gap: var(--aq-gap); align-items: start;
}
@media (max-width: 1100px) {
  .step-answer-question .aq-panes { grid-template-columns: minmax(0, 1fr); }
  .step-answer-question .aq-rail { position: static !important; max-height: none !important; }
}

.step-answer-question .aq-context {
  background: var(--card, #fff); border: 1px solid var(--border, #e7e5e4);
  border-radius: 8px; padding: 24px 28px;
  max-height: 74vh; overflow-y: auto;
}
.step-answer-question .aq-context-inner { max-width: 68ch; }
.step-answer-question .aq-context-inner > * { max-width: 100%; }
.step-answer-question .aq-context-inner p,
.step-answer-question .aq-context-inner li {
  font-size: 15px; line-height: 1.7; color: var(--text, #1c1917);
}
.step-answer-question .aq-context-inner h1 { font-size: 20px; margin: 0 0 6px; line-height: 1.3; }
.step-answer-question .aq-context-inner h2 { font-size: 16px; margin: 24px 0 8px; line-height: 1.35; }
.step-answer-question .aq-context-inner h3 { font-size: 14px; margin: 18px 0 6px; line-height: 1.4; }
.step-answer-question .aq-context-inner code {
  font-size: 0.9em; padding: 1px 4px; border-radius: 3px; background: var(--bg, #f5f5f4);
}
.step-answer-question .aq-context-inner pre {
  background: var(--bg, #f5f5f4); padding: 12px 14px; border-radius: 6px;
  overflow-x: auto; font-size: 13px; line-height: 1.55;
}
.step-answer-question .aq-context-inner pre code { background: none; padding: 0; }
.step-answer-question .aq-context-inner blockquote {
  margin: 16px 0; padding: 2px 0 2px 16px;
  border-left: 3px solid var(--accent, #2563eb); color: var(--text-dim, #78716c);
}
.step-answer-question .aq-empty-context {
  color: var(--text-dim, #78716c); font-size: 14px; line-height: 1.6; font-style: italic;
}

.step-answer-question .aq-rail {
  position: sticky; top: 12px; max-height: 74vh; overflow-y: auto;
  display: flex; flex-direction: column; gap: 14px;
}
.step-answer-question .aq-card {
  border: 1px solid var(--border, #e7e5e4); border-radius: 6px;
  padding: 14px 16px; background: var(--card, #fff);
}
.step-answer-question .aq-label {
  display: block; font-size: 11px; font-weight: 600; letter-spacing: .04em;
  text-transform: uppercase; color: var(--text-dim, #78716c); margin: 0 0 6px;
}
.step-answer-question .aq-question {
  font-size: 14px; line-height: 1.6; color: var(--text, #1c1917); white-space: pre-wrap;
}

/* The verdict is three explicit choices, not a dropdown: the whole
   protocol forks on it, and a fork you can mis-scroll is a fork you
   can answer by accident. */
.step-answer-question .aq-verdicts { display: flex; gap: 8px; flex-wrap: wrap; }
.step-answer-question .aq-verdict {
  flex: 1 1 auto; font: inherit; font-size: 13px; cursor: pointer;
  padding: 7px 10px; border-radius: 5px; text-align: center;
  border: 1px solid var(--border, #e7e5e4);
  background: var(--bg, #fafaf9); color: var(--text, #1c1917);
}
.step-answer-question .aq-verdict:hover:not(:disabled) { border-color: var(--accent, #2563eb); }
.step-answer-question .aq-verdict.is-on {
  border-color: var(--accent, #2563eb); background: var(--accent, #2563eb); color: #fff;
}
.step-answer-question .aq-verdict.is-on.aq-declined { border-color: #b91c1c; background: #b91c1c; }
.step-answer-question .aq-verdict:disabled { opacity: .5; cursor: default; }

.step-answer-question .aq-proposal {
  padding: 8px 10px; border-radius: 5px;
  border: 1px solid var(--border, #e7e5e4);
  border-left: 3px solid var(--accent, #2563eb);
  background: var(--bg, #fafaf9);
}
.step-answer-question .aq-proposal-head {
  display: flex; align-items: center; justify-content: space-between; gap: 8px;
}
.step-answer-question .aq-proposal-text {
  margin-top: 6px; font-size: 13px; line-height: 1.55;
  color: var(--text, #1c1917); white-space: pre-wrap;
}
.step-answer-question .aq-use {
  flex: none; font: inherit; font-size: 12px; cursor: pointer;
  padding: 3px 10px; border-radius: 4px;
  border: 1px solid var(--accent, #2563eb);
  background: transparent; color: var(--accent, #2563eb);
}
.step-answer-question .aq-use:hover:not(:disabled) { background: var(--accent, #2563eb); color: #fff; }
.step-answer-question .aq-use:disabled { opacity: .5; cursor: default; }

.step-answer-question .aq-card textarea {
  width: 100%; box-sizing: border-box; resize: vertical;
  font: inherit; font-size: 13px; line-height: 1.55;
  padding: 8px 10px; border-radius: 5px;
  border: 1px solid var(--border, #e7e5e4);
  background: var(--bg, #fafaf9); color: var(--text, #1c1917);
}
.step-answer-question .aq-card textarea:focus {
  outline: 2px solid var(--accent, #2563eb); outline-offset: -1px; background: var(--card, #fff);
}
.step-answer-question .aq-card textarea:disabled { opacity: .7; }

.step-answer-question .aq-actions {
  display: flex; align-items: center; gap: 12px; flex-wrap: wrap;
  margin-top: var(--aq-gap); padding-top: 14px;
  border-top: 1px solid var(--border, #e7e5e4);
}
.step-answer-question .aq-submit {
  font: inherit; font-size: 13px; font-weight: 600; cursor: pointer;
  padding: 8px 16px; border-radius: 5px; border: 1px solid var(--accent, #2563eb);
  background: var(--accent, #2563eb); color: #fff;
}
.step-answer-question .aq-submit:disabled { opacity: .45; cursor: default; }
.step-answer-question .aq-hint { font-size: 12px; color: var(--text-dim, #78716c); }
.step-answer-question .aq-error {
  padding: 10px 12px; border-radius: 6px; font-size: 13px; line-height: 1.5;
  background: #fef2f2; color: #b91c1c; border: 1px solid #fecaca;
}
.step-answer-question .aq-done {
  padding: 12px 14px; border-radius: 6px; font-size: 13px; line-height: 1.6;
  background: #f0fdf4; color: #15803d; border: 1px solid #bbf7d0;
}
`;

  function injectStyles() {
    if (document.getElementById(STYLE_ID)) return;
    const el = document.createElement('style');
    el.id = STYLE_ID;
    el.textContent = STYLES;
    document.head.appendChild(el);
  }

  function h(tag, props, ...kids) {
    const el = document.createElement(tag);
    Object.entries(props || {}).forEach(([k, v]) => {
      if (k === 'className') el.className = v;
      else if (k === 'disabled') { if (v) el.setAttribute('disabled', ''); }
      else if (v !== null && v !== undefined) el.setAttribute(k, v);
    });
    kids.flat().forEach((c) => {
      if (c === null || c === undefined || c === false) return;
      el.appendChild(typeof c === 'string' ? document.createTextNode(c) : c);
    });
    return el;
  }

  // -----------------------------------------------------------------
  // Markdown, deliberately small.
  //
  // A plugin bundle has no dependencies — the gateway serves it as a
  // static asset and a CDN import would be a second trust boundary for
  // a panel that renders text somebody else wrote. So this covers what
  // evidence actually uses: headings, fenced and inline code, lists,
  // quotes, links, bold/italic, rules. Anything else degrades to a
  // paragraph, which is a legible failure rather than a broken one.
  //
  // HTML is escaped FIRST and the transforms only ever emit tags this
  // function itself wrote. The context is asker-supplied, and an asker
  // is not always the person answering.
  // -----------------------------------------------------------------
  function escapeHtml(s) {
    return s
      .replace(/&/g, '&amp;')
      .replace(/</g, '&lt;')
      .replace(/>/g, '&gt;')
      .replace(/"/g, '&quot;');
  }

  function renderInline(s) {
    return s
      .replace(/`([^`]+)`/g, (_, c) => `<code>${c}</code>`)
      .replace(/\*\*([^*]+)\*\*/g, '<strong>$1</strong>')
      .replace(/(^|[^*])\*([^*\n]+)\*/g, '$1<em>$2</em>')
      // Only http(s) — a link is the one place a transform could emit
      // a scheme, and javascript: is the reason to name the allowed one.
      .replace(/\[([^\]]+)\]\((https?:\/\/[^\s)]+)\)/g,
        '<a href="$2" target="_blank" rel="noopener noreferrer">$1</a>');
  }

  function renderMarkdown(md) {
    const src = escapeHtml(String(md).replace(/\r\n/g, '\n'));
    const lines = src.split('\n');
    const out = [];
    let i = 0;
    let para = [];
    let list = null; // 'ul' | 'ol'

    const flushPara = () => {
      if (para.length) {
        out.push(`<p>${renderInline(para.join(' '))}</p>`);
        para = [];
      }
    };
    const flushList = () => {
      if (list) { out.push(`</${list}>`); list = null; }
    };

    while (i < lines.length) {
      const line = lines[i];

      // Fenced code — verbatim, no inline transforms inside.
      const fence = line.match(/^```(\w*)\s*$/);
      if (fence) {
        flushPara(); flushList();
        const body = [];
        i += 1;
        while (i < lines.length && !/^```\s*$/.test(lines[i])) { body.push(lines[i]); i += 1; }
        i += 1;
        out.push(`<pre><code>${body.join('\n')}</code></pre>`);
        continue;
      }

      if (/^\s*$/.test(line)) { flushPara(); flushList(); i += 1; continue; }

      const head = line.match(/^(#{1,3})\s+(.*)$/);
      if (head) {
        flushPara(); flushList();
        out.push(`<h${head[1].length}>${renderInline(head[2])}</h${head[1].length}>`);
        i += 1; continue;
      }

      if (/^(-{3,}|\*{3,})\s*$/.test(line)) {
        flushPara(); flushList(); out.push('<hr />'); i += 1; continue;
      }

      const quote = line.match(/^&gt;\s?(.*)$/);
      if (quote) {
        flushPara(); flushList();
        out.push(`<blockquote>${renderInline(quote[1])}</blockquote>`);
        i += 1; continue;
      }

      const ul = line.match(/^\s*[-*]\s+(.*)$/);
      const ol = line.match(/^\s*\d+\.\s+(.*)$/);
      if (ul || ol) {
        flushPara();
        const want = ul ? 'ul' : 'ol';
        if (list !== want) { flushList(); out.push(`<${want}>`); list = want; }
        out.push(`<li>${renderInline((ul || ol)[1])}</li>`);
        i += 1; continue;
      }

      para.push(line.trim());
      i += 1;
    }
    flushPara(); flushList();
    return out.join('\n');
  }

  const VERDICTS = [
    { key: 'approved', label: 'Approve' },
    { key: 'declined', label: 'Decline' },
    { key: 'answered', label: 'Answer' },
  ];

  function mount(container, { step, jobId, onUpdate }) {
    injectStyles();
    const root = h('div', { className: 'step-answer-question' });
    container.replaceChildren(root);

    const isDone = step.status === 'completed' || step.status === 'skipped';
    let verdict = (step.metadata && step.metadata.verdict) || '';
    let answer = (step.metadata && step.metadata.answer) || '';
    let job = null;
    let saving = false;
    let error = null;

    const body = h('div', {});
    root.appendChild(body);

    function meta(key) {
      // Step first, Job second, the routing step third: the step's
      // brief is written for THIS decision; the Job's fields are the
      // packet-wide fallback; and on a triaged packet (backlog-item,
      // user-feedback) the brief is written by whoever chose the
      // `design` route, in the same write as the disposition — so it
      // sits on that completed step, not on this one. Reading it here
      // is what lets the route and its brief be one act (2026-09-05:
      // three items reached the decider with the brief a step away).
      const own =
        step.metadata && typeof step.metadata[key] === 'string'
          ? step.metadata[key].trim()
          : '';
      if (own) return own;
      const packet =
        job && job.metadata && typeof job.metadata[key] === 'string'
          ? job.metadata[key].trim()
          : '';
      if (packet) return packet;
      const steps = (job && Array.isArray(job.steps)) ? job.steps : [];
      const router = steps.find(
        (s) =>
          s && s.id !== step.id && s.status === 'completed' &&
          s.metadata && typeof s.metadata.disposition === 'string' &&
          typeof s.metadata[key] === 'string' && s.metadata[key].trim(),
      );
      return router ? router.metadata[key].trim() : '';
    }

    function render() {
      body.replaceChildren();
      if (error) body.appendChild(h('div', { className: 'aq-error' }, error));

      const asker = meta('asked_by');
      body.appendChild(
        h('div', { className: 'aq-head' },
          h('h3', {}, (job && job.title) || 'Answer a question'),
          asker ? h('span', { className: 'aq-asker' }, `asked by ${asker}`) : null,
        ),
      );

      if (isDone) {
        body.appendChild(
          h('div', { className: 'aq-done' },
            `Answered — ${verdict || 'recorded'}. ${answer}`),
        );
        return;
      }

      const panes = h('div', { className: 'aq-panes' });

      // ---- Left: the context the asker supplied. ----
      const ctx = meta('context_md');
      const ctxPane = h('div', { className: 'aq-context' });
      if (ctx) {
        const inner = h('div', { className: 'aq-context-inner' });
        inner.innerHTML = renderMarkdown(ctx);
        ctxPane.appendChild(inner);
      } else {
        // Named, not blank: an approval with no evidence is a request
        // to guess, and the asker should see that reflected back.
        ctxPane.appendChild(
          h('div', { className: 'aq-empty-context' },
            'The asker supplied no context. Answering this means taking the question on trust — worth declining and asking for evidence if the call matters.'),
        );
      }
      panes.appendChild(ctxPane);

      // ---- Right: the decision. ----
      const rail = h('div', { className: 'aq-rail' });

      // A feedback packet rarely carries a `question` key — the filed
      // message is the question. Chain, never blank.
      const q = meta('question') || meta('message') || meta('body');
      rail.appendChild(
        h('div', { className: 'aq-card' },
          h('span', { className: 'aq-label' }, 'The question'),
          h('div', { className: 'aq-question' }, q || '(no question recorded)'),
        ),
      );

      const ta = h('textarea', {
        rows: 6,
        placeholder: 'What you decided, and why…',
        disabled: isDone,
      });
      ta.value = answer;
      ta.addEventListener('input', (e) => { answer = e.target.value; refreshActions(); });

      const verdictRow = h('div', { className: 'aq-verdicts' });
      VERDICTS.forEach((v) => {
        const b = h('button', {
          type: 'button',
          className: `aq-verdict aq-${v.key}${verdict === v.key ? ' is-on' : ''}`,
          disabled: isDone,
        }, v.label);
        b.addEventListener('click', () => {
          verdict = v.key;
          Array.from(verdictRow.children).forEach((c) => c.classList.remove('is-on'));
          b.classList.add('is-on');
          refreshActions();
        });
        verdictRow.appendChild(b);
      });

      const decide = h('div', { className: 'aq-card' },
        h('span', { className: 'aq-label' }, 'Your verdict'),
        verdictRow,
      );

      const proposed = meta('proposed');
      if (proposed) {
        const useBtn = h('button', { className: 'aq-use', type: 'button', disabled: isDone }, 'Use this');
        useBtn.addEventListener('click', () => {
          ta.value = proposed;
          answer = proposed;
          refreshActions();
          ta.focus();
        });
        decide.appendChild(
          h('div', { className: 'aq-proposal' },
            h('div', { className: 'aq-proposal-head' },
              h('span', { className: 'aq-label' }, 'Proposed by the asker'),
              useBtn),
            h('div', { className: 'aq-proposal-text' }, proposed),
          ),
        );
      }

      decide.appendChild(h('span', { className: 'aq-label' }, 'Your answer'));
      decide.appendChild(ta);
      rail.appendChild(decide);
      panes.appendChild(rail);
      body.appendChild(panes);

      body.appendChild(actions);
      refreshActions();
    }

    const actions = h('div', { className: 'aq-actions' });
    function refreshActions() {
      actions.replaceChildren();
      if (isDone) return;
      const ready = verdict !== '' && answer.trim().length > 0;
      const btn = h('button', {
        className: 'aq-submit', type: 'button', disabled: !ready || saving,
      }, saving ? 'Recording…' : 'Record the answer');
      btn.addEventListener('click', submit);
      actions.appendChild(btn);
      actions.appendChild(
        h('span', { className: 'aq-hint' },
          ready
            ? 'Completing this closes the packet on the terminal your verdict names.'
            : 'Pick a verdict and write an answer — both are required, so the reasoning is on the record.'),
      );
    }

    async function submit() {
      saving = true; error = null; refreshActions();
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
          body: JSON.stringify({ verdict, answer }),
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
          body: JSON.stringify({ ...fresh, job_id: jobId, status: 'completed' }),
        });
        if (!r.ok) throw new Error(`HTTP ${r.status}: ${await r.text()}`);
        if (typeof onUpdate === 'function') onUpdate();
      } catch (e) {
        error = e instanceof Error ? e.message : String(e);
        saving = false;
        render();
      }
    }

    (async () => {
      try {
        const r = await fetch(`/api/jobs/${jobId}`);
        if (r.ok) job = await r.json();
      } catch (e) {
        // The question and context live on the Job; without it the
        // surface can still record an answer, so degrade rather than
        // block.
        console.warn('[answer-question] job fetch failed:', e);
      }
      render();
    })();

    return () => container.replaceChildren();
  }

  if (typeof window.__boss_register_step_plugin !== 'function') {
    console.error('[answer-question-plugin] __boss_register_step_plugin not on window');
    return;
  }
  window.__boss_register_step_plugin('answer-question', mount);
})();
