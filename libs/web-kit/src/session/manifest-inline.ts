// The tenant manifest's pure half: its state type, the wire shape the
// gateway serves at `/api/tenant/manifest` and inlines into index.html
// as `window.__BOSS_TENANT_MANIFEST__`, and the reader that turns the
// document-carried value into a `ready` state before first paint
// (5578e42d). Plain TypeScript, no runes, so `bun test` can exercise
// it; manifest.svelte.ts owns the reactive store and re-exports these.

export type ManifestState =
  | { kind: 'loading' }
  | {
      kind: 'ready';
      /// What the tenant calls itself, from tenant.toml `[meta]`.
      /// Undefined for a deployment that has not named itself.
      displayName?: string;
      tenantId?: string;
      modules: Readonly<Record<string, boolean>>;
      labels: Readonly<Record<string, string>>;
    }
  | { kind: 'error' };

/// The shape the gateway serves at `/api/tenant/manifest` and inlines
/// into index.html as `window.__BOSS_TENANT_MANIFEST__` (5578e42d).
export type ManifestBody = {
  display_name?: string;
  tenant_id?: string;
  modules?: Record<string, boolean>;
  labels?: Record<string, string>;
};

export function readyFrom(body: ManifestBody): ManifestState {
  return {
    kind: 'ready',
    displayName: body.display_name,
    tenantId: body.tenant_id,
    modules: body.modules ?? {},
    labels: body.labels ?? {},
  };
}

/// The manifest the document carried, if the gateway put one there —
/// `ready` before the first paint, so the shell never renders a label
/// or a module the tenant overrides. Anything that is not a manifest
/// object (absent, null, a string, a stray number) is `null`: the
/// fetch path stays the fallback, never a crash at import time.
export function manifestFromInline(raw: unknown): ManifestState | null {
  if (raw === null || typeof raw !== 'object' || Array.isArray(raw)) return null;
  const body = raw as Record<string, unknown>;
  const modules = body.modules;
  const labels = body.labels;
  if (modules !== undefined && (typeof modules !== 'object' || modules === null)) return null;
  if (labels !== undefined && (typeof labels !== 'object' || labels === null)) return null;
  return readyFrom(body as ManifestBody);
}

