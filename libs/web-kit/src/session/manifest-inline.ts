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

/// Is `module_id` on for this tenant? ON only when the manifest lists
/// it `true` (ce68f137). The previous rule — off only on an explicit
/// `false` — meant a tenant that listed nothing got everything: on
/// 2026-09-17 prod's `modules = {}` was showing Algedonic, LLC the
/// playground's Simulator tab, storefront and QA hub. A tenant now
/// declares what it uses; the names are in docs/tenant-contract.md.
///
/// A manifest that is still loading or unreachable hides nothing.
/// That is a deployment fault, not a tenant decision, and a blank
/// shell would hide the fault instead of showing it; the gateway
/// inlines the manifest into index.html, so on a served page this
/// state does not reach the first paint.
export function moduleOn(state: ManifestState, module_id: string): boolean {
  if (state.kind !== 'ready') return true;
  return state.modules[module_id] === true;
}

/// The tab title: the tenant's own name, or the product's until the
/// tenant has named itself (ce68f137). index.html ships the same
/// neutral default so a cold load before the manifest is never the
/// playground tenant.
export function documentTitleFor(state: ManifestState): string {
  if (state.kind !== 'ready' || !state.displayName) return 'BOSS';
  return state.displayName;
}

/// The meta description, in the same two shapes as the title.
export function documentDescriptionFor(state: ManifestState): string {
  const product = 'BOSS — software for modeling systems as state machines';
  if (state.kind !== 'ready' || !state.displayName) return `${product}.`;
  return `${state.displayName}, running on ${product}.`;
}

