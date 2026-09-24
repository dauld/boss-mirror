<script lang="ts">
  // Rendered when a route resolves but the tenant manifest has the
  // matching module flagged false. Better than rendering an empty
  // page (looks broken) or 404 (looks like a bug). Says clearly
  // that the module is off for this tenant + offers a return-home
  // link.

  import { navigate } from '../router';

  type Props = {
    module: string;
    label?: string;
  };
  let { module, label }: Props = $props();
</script>

<div class="module-disabled">
  <div class="card">
    <h1>Not enabled for this tenant</h1>
    <p>
      The <strong>{label ?? module}</strong> module is turned off in
      this tenant's <code>tenant.toml</code>. The page exists in the
      platform — the active tenant just doesn't surface it.
    </p>
    <p class="muted">
      To enable: set <code>{module} = true</code> in
      <code>examples/&lt;tenant&gt;/seeds/tenant.toml</code> under
      <code>[modules]</code>, redeploy, and the page comes back.
    </p>
    <button
      type="button"
      class="primary"
      onclick={(e) => {
        e.preventDefault();
        navigate('/');
      }}
    >
      Back to home
    </button>
  </div>
</div>

<style>
  .module-disabled {
    display: flex;
    align-items: flex-start;
    justify-content: center;
    padding: 60px 20px;
  }
  .card {
    max-width: 520px;
    background: var(--ink);
    border: 1px solid var(--hairline);
    border-radius: 8px;
    padding: 32px;
  }
  h1 {
    margin: 0 0 16px;
    font-size: 22px;
    color: var(--fog);
  }
  p {
    margin: 0 0 14px;
    line-height: 1.5;
    color: var(--static);
  }
  .muted {
    color: var(--static);
    font-size: 14px;
  }
  code {
    background: var(--ink-raised);
    padding: 1px 5px;
    border-radius: 3px;
    font-size: 13px;
  }
  button.primary {
    margin-top: 8px;
    padding: 8px 16px;
    background: var(--band);
    color: var(--on-band);
    border: none;
    border-radius: 4px;
    cursor: pointer;
    font-weight: 500;
  }
  button.primary:hover {
    background: var(--signal);
  }
</style>
