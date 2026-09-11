// Tenant module manifest — which platform surfaces this tenant uses
// + per-tenant terminology overrides.
//
// The dev-server + the production gateway both read `[modules]` and
// `[labels]` out of the active tenant.toml and serve them at
// `/api/tenant/manifest`. The SPA fetches once on load and gates
// sidebar entries against modules + resolves tenant-specific
// vocabulary against labels.
//
// Treat modules as advisory: a `false` value means "hide this surface
// in the nav and skip its sim generators." It does not unload any
// service crate — every platform service stays running and can be
// hit directly by URL if a power user wants to.
//
// Labels are pure presentation: the brewery's `assets.entity_singular =
// "vessel"` doesn't change what's stored, only what the SPA prints.
//
// `[meta].display_name` rides along for the same reason: the chrome
// had the brewery's name hardcoded at three render sites, so a second
// tenant rendered someone else's brand in its top bar.

import {
  manifestFromInline,
  readyFrom,
  type ManifestBody,
  type ManifestState,
} from './manifest-inline';

export { manifestFromInline, type ManifestState };

const inlined = manifestFromInline(
  (globalThis as { __BOSS_TENANT_MANIFEST__?: unknown }).__BOSS_TENANT_MANIFEST__,
);

export const manifest = $state<{ value: ManifestState }>({
  value: inlined ?? { kind: 'loading' },
});

export async function loadManifest(): Promise<void> {
  try {
    const r = await fetch('/api/tenant/manifest');
    if (!r.ok) {
      manifest.value = { kind: 'error' };
      return;
    }
    const body = (await r.json()) as ManifestBody;
    manifest.value = readyFrom(body);
  } catch {
    manifest.value = { kind: 'error' };
  }
}

/// True if `module_id` is enabled by the tenant manifest. While the
/// manifest is loading or unreachable, default to `true` — a missing
/// manifest shouldn't blank out the entire UI. Once a tenant has
/// explicitly listed `module_id = false`, that wins.
export function moduleEnabled(module_id: string): boolean {
  if (manifest.value.kind !== 'ready') return true;
  const v = manifest.value.modules[module_id];
  return v !== false;
}

/// Resolve a tenant-configurable label. Falls back to the supplied
/// default when the manifest isn't loaded or doesn't override the
/// key. Use sparingly — only where a specific tenant has a
/// meaningfully better word for a generic concept ("kegs" vs
/// "devices"). The goal is to fix presentation lies, not to make
/// every string in the SPA configurable.
export function getLabel(key: string, fallback: string): string {
  if (manifest.value.kind !== 'ready') return fallback;
  return manifest.value.labels[key] ?? fallback;
}
