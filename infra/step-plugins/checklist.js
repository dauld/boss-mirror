// checklist.js — renders step.metadata.items as a checkbox list,
// auto-completes the step when every box is checked.
//
// Plugins are plain-DOM mount functions. The host (StepPluginMount
// .svelte) creates a container <div> and calls mount(container, props);
// we render into it and return a cleanup fn. No framework runtime.

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
    const items = Array.isArray(step.metadata && step.metadata.items)
      ? step.metadata.items.map((i) => ({
          label: String(i.label || ''),
          checked: !!i.checked,
        }))
      : [];
    let saving = false;
    const isDone = step.status === 'done';

    function allChecked() {
      return items.length > 0 && items.every((i) => i.checked);
    }
    function checkedCount() {
      return items.filter((i) => i.checked).length;
    }

    const progressSpan = h('span', { className: 'step-checklist-progress' });
    const itemsDiv = h('div', { className: 'step-checklist-items' });
    const actionsDiv = h('div', { className: 'step-actions' });

    const saveBtn = h('button', { className: 'step-btn' }, 'Save');
    const completeBtn = h(
      'button',
      { className: 'step-btn step-btn-primary' },
      'All checked — complete step',
    );
    saveBtn.addEventListener('click', () => save(false));
    completeBtn.addEventListener('click', () => save(true));

    function renderItems() {
      itemsDiv.replaceChildren();
      items.forEach((item, idx) => {
        const row = h(
          'label',
          {
            className: `step-checklist-item ${item.checked ? 'step-checklist-checked' : ''}`,
          },
          h('input', {
            type: 'checkbox',
            checked: item.checked,
            disabled: isDone,
            onChange: () => toggle(idx),
          }),
          h('span', null, item.label),
        );
        itemsDiv.appendChild(row);
      });
    }

    function renderActions() {
      actionsDiv.replaceChildren();
      if (isDone) return;
      saveBtn.disabled = saving;
      actionsDiv.appendChild(saveBtn);
      if (allChecked()) {
        completeBtn.disabled = saving;
        actionsDiv.appendChild(completeBtn);
      }
    }

    function renderProgress() {
      progressSpan.textContent = `${checkedCount()}/${items.length}`;
    }

    function toggle(idx) {
      items[idx] = { ...items[idx], checked: !items[idx].checked };
      renderItems();
      renderProgress();
      renderActions();
    }

    async function save(autoComplete) {
      saving = true;
      renderActions();
      try {
        const completing = autoComplete && allChecked();
        // Merge ONLY the key this surface owns. The old idiom PUT
        // `{ ...step.metadata, items }` — the page-load snapshot plus
        // our key — and PUT metadata is replaced WHOLESALE, so any key
        // another writer added after this page loaded was silently
        // erased (the lost update the step metadata PATCH exists to
        // retire). The server merges against the row as it stands and
        // preserves every key this body does not name.
        const pr = await fetch(`/api/jobs/${jobId}/steps/${step.id}/metadata`, {
          method: 'PATCH',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ items }),
        });
        // The old single PUT went unchecked; with two writes the
        // follow-up must not fire when the merge it depends on failed,
        // or a failed save could still move the status. The host
        // refresh below still runs either way, so a silent failure
        // shows server truth — exactly what it showed before.
        if (pr.ok) {
          if (completing) {
            // The PATCH answers 204 with no body, and the completion
            // PUT replaces metadata wholesale — read the post-merge
            // row back and complete with the true final shape, never
            // the snapshot. (No single-step GET exists; the job's
            // steps list is the read the API offers.)
            const lr = await fetch(`/api/jobs/${jobId}/steps`);
            const stepsNow = lr.ok ? await lr.json() : null;
            const fresh = Array.isArray(stepsNow)
              ? stepsNow.find((s) => s.id === step.id)
              : null;
            if (fresh) {
              await fetch(`/api/jobs/${jobId}/steps/${step.id}`, {
                method: 'PUT',
                headers: { 'Content-Type': 'application/json' },
                body: JSON.stringify({ ...fresh, job_id: jobId, status: 'done' }),
              });
            }
          } else if (step.status === 'pending') {
            // A save still flips a pending step active. Status cannot
            // travel through the metadata PATCH, and the step PUT
            // overlays — every field this body omits keeps its current
            // value, the just-merged metadata included.
            await fetch(`/api/jobs/${jobId}/steps/${step.id}`, {
              method: 'PUT',
              headers: { 'Content-Type': 'application/json' },
              body: JSON.stringify({ job_id: jobId, status: 'active' }),
            });
          }
        }
        onUpdate();
      } finally {
        saving = false;
        renderActions();
      }
    }

    const root = h(
      'div',
      { className: 'step-surface step-checklist' },
      h(
        'div',
        { className: 'step-surface-header' },
        h('h3', null, step.title),
        h('span', { className: `step-status step-status-${step.status}` }, step.status),
        progressSpan,
      ),
      itemsDiv,
      actionsDiv,
    );

    renderItems();
    renderProgress();
    renderActions();
    container.appendChild(root);

    return function cleanup() {
      root.remove();
    };
  }

  if (typeof window.__boss_register_step_plugin !== 'function') {
    console.error('[checklist-plugin] __boss_register_step_plugin not on window');
    return;
  }
  window.__boss_register_step_plugin('checklist', mount);
})();
