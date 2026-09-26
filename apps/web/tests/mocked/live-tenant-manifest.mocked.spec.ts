// The recorded live tenant manifest (live-tenant-manifest.json) is the
// ONE place a mocked spec learns what the live tenant calls itself
// (backlog af138621). installTenantManifest sent 'Algedonic Ales' /
// 'brewery' — a tenant the instance stopped being — while four specs
// kept their own inlined copy of the live name and accounts two more,
// so a rename on the instance meant six edits, and a recording that
// changed moved none of them. These two legs pin the collapse: the
// served manifest IS the recording, and no spec types its name.

import { readdirSync, readFileSync } from 'fs';
import { expect, test } from '@playwright/test';
import { mountPage } from './_helpers';
import { installSmokeMocks, installTenantManifest, MODULES_LIVE, tenantManifest } from './_smokeMocks';
import LIVE_RECORDING from './live-tenant-manifest.json' with { type: 'json' };

const NAME = LIVE_RECORDING.body.display_name;
const DIR = new URL('./', import.meta.url);

test.describe('the recorded live tenant manifest', () => {
  test('installTenantManifest serves the recording, name and id included', async ({ page }) => {
    expect(tenantManifest(MODULES_LIVE)).toMatchObject({
      display_name: NAME, tenant_id: LIVE_RECORDING.body.tenant_id, modules: MODULES_LIVE,
    });
    await installSmokeMocks(page);
    await installTenantManifest(page, MODULES_LIVE, { inline: true });
    await mountPage(page, '/');
    // documentTitleFor: the tab carries the tenant's own name.
    await expect(page).toHaveTitle(NAME);
  });

  test(`no mocked spec types the recorded name '${NAME}' itself`, () => {
    const typed = readdirSync(DIR)
      .filter((f) => f.endsWith('.ts'))
      .filter((f) => readFileSync(new URL(f, DIR), 'utf8').includes(`'${NAME}'`));
    expect(typed, 'read it from live-tenant-manifest.json through _smokeMocks').toEqual([]);
  });
});
