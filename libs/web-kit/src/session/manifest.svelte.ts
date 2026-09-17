// Tenant module manifest — which platform surfaces this tenant uses
// + per-tenant terminology overrides.
//
// The dev-server + the production gateway both read `[modules]` and
// `[labels]` out of the active tenant.toml and serve them at
// `/api/tenant/manifest`. The SPA fetches once on load and gates
// sidebar entries against modules + resolves tenant-specific
// vocabulary against labels.
//
// Treat modules as advisory: a module is on only when the tenant lists
// it `true` (ce68f137) — off means "hide this surface in the nav,
// answer its routes with the module-off page and skip its sim
// generators." It does not unload any service crate — every platform
// service stays running and its API can be hit directly.
//
// Labels are pure presentation: the brewery's `assets.entity_singular =
// "vessel"` doesn't change what's stored, only what the SPA prints.
//
// `[meta].display_name` rides along for the same reason: the chrome
// had the brewery's name hardcoded at three render sites, so a second
// tenant rendered someone else's brand in its top bar.

import {
  documentDescriptionFor,
  documentTitleFor,
  manifestFromInline,
  moduleOn,
  readyFrom,
  type ManifestBody,
  type ManifestState,
} from './manifest-inline';

export { manifestFromInline, moduleOn, type ManifestState };

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

/// True if the tenant manifest lists `module_id = true`. The rule, and
/// its one exception (a manifest not yet loaded), are `moduleOn`'s.
export function moduleEnabled(module_id: string): boolean {
  return moduleOn(manifest.value, module_id);
}

/// Put the tenant on the document: the tab title, og:title and the two
/// descriptions follow the manifest, so the shell never advertises the
/// playground tenant on a deployment that is not it (ce68f137).
/// Idempotent; apps/web calls it whenever the manifest settles. The
/// simulator host keeps its own index.html title, which names the
/// simulator rather than the tenant.
export function reflectTenantOnDocument(): void {
  if (typeof document === 'undefined') return;
  const title = documentTitleFor(manifest.value);
  const description = documentDescriptionFor(manifest.value);
  document.title = title;
  document.querySelector('meta[property="og:title"]')?.setAttribute('content', title);
  for (const selector of ['meta[name="description"]', 'meta[property="og:description"]']) {
    document.querySelector(selector)?.setAttribute('content', description);
  }
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
