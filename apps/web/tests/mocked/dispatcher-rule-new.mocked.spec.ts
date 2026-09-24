// /it/registry/rules — "+ New rule" points at the two paths a rule
// survives a restart on, and makes no rule itself (backlog 7d9df2fe,
// design ff1c3615: David chose option b, 2026-09-23).
//
// WHY. The editor's create sent no `source`, so its rule was a PRODUCT
// rule, and the dispatcher's boot seed (rules::seed) retires every
// active product rule no file under infra/dispatcher/rules/ names. The
// page's primary control therefore delivered a rule that lived until
// the next restart, and its retirement showed only in a boot log. The
// server now refuses that draft too; this spec pins the page half: the
// control lands on guidance naming both durable paths, there is no form
// to submit, and nothing is ever POSTed to the draft door from it.

import { test, expect } from '@playwright/test';
import { mountPage } from './_helpers';
import { installSmokeMocks } from './_smokeMocks';

test.beforeEach(async ({ page }) => {
  await installSmokeMocks(page);
});

test('+ New rule names the durable paths and creates nothing', async ({ page }) => {
  const drafts: string[] = [];
  page.on('request', (req) => {
    if (req.method() === 'POST' && /\/api\/dispatcher\/rules$/.test(req.url())) {
      drafts.push(req.url());
    }
  });

  await mountPage(page, '/it/registry/rules');
  await Promise.all([
    page.waitForURL(/\/it\/registry\/rules\/new$/),
    page.getByRole('link', { name: '+ New rule' }).first().click(),
  ]);

  const main = page.locator('.catalog');
  await expect(main).toContainText('infra/dispatcher/rules/');
  await expect(main).toContainText('seeds/rules.toml');
  await expect(main).toContainText(/retire/i);

  // No create form survives on this route: nothing to name, nothing to save.
  await expect(page.getByRole('button', { name: /save draft/i })).toHaveCount(0);
  await expect(page.getByPlaceholder('advance-dag-on-step-done')).toHaveCount(0);
  expect(drafts, 'the new-rule page must never POST a draft').toEqual([]);
});
