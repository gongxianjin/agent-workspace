'use strict';
// Actual local detection and HTTP UI, no setup fixture, model command or distribution acceptance.
const fs = require('node:fs');
const path = require('node:path');
const assert = require('node:assert/strict');
const [launchFile, output, playwrightArg, chromiumArg] = process.argv.slice(2);
const { chromium } = require(path.dirname(playwrightArg));
(async () => {
  const { url } = JSON.parse(fs.readFileSync(launchFile, 'utf8'));
  const browser = await chromium.launch({ headless: true, executablePath: chromiumArg });
  const results = [], errors = [], modelRequests = [], acceptanceRequests = [];
  try {
    const context = await browser.newContext({ viewport: { width: 1440, height: 1000 } });
    const page = await context.newPage();
    page.on('pageerror', error => errors.push(String(error)));
    page.on('request', request => {
      if (request.method() !== 'POST') return;
      const pathname = new URL(request.url()).pathname;
      if (pathname === '/api/command') modelRequests.push(request.postDataJSON().kind);
      if (pathname === '/api/setup/distribution') acceptanceRequests.push(pathname);
    });
    async function check(name, run) {
      try { await run(); results.push({ name, passed: true }); }
      catch (error) { results.push({ name, passed: false, error: String(error) }); throw error; }
    }
    await page.goto(url);
    await page.waitForFunction(() => document.body.dataset.connected === 'true');
    await check('fresh-real-setup-explains-required-project-without-detection', async () => {
      assert.equal(await page.locator('#generate-document').isDisabled(), true);
      assert.match(await page.locator('#generate-hint').innerText(), /新建.*项目/);
      assert.match(await page.locator('#setup-status').innerText(), /未检测/);
      await page.locator('#new-project-name').fill('免费首次接入验证');
      await page.locator('#create-project').click();
      await page.waitForFunction(() => document.body.dataset.project?.length === 32);
      assert.equal(await page.locator('#generate-document').isDisabled(), true);
      assert.match(await page.locator('#generate-hint').innerText(), /重新检测/);
    });
    await check('real-fixed-command-detection-cannot-claim-model-validation', async () => {
      await page.locator('#setup-recheck').click();
      await page.waitForFunction(() => !document.querySelector('#setup-recheck').disabled
        && document.querySelector('#setup-status').textContent.includes('本地自检通过'), null, { timeout: 90000 });
      assert.match(await page.locator('#setup-status').innerText(), /版本：\d+\.\d+\.\d+/);
      assert.match(await page.locator('#setup-status').innerText(), /支持必需参数/);
      assert.match(await page.locator('#setup-status').innerText(), /实际模型接入未验证/);
      assert.equal(await page.locator('#generate-document').isDisabled(), true);
      assert.match(await page.locator('#generate-hint').innerText(), /使用已有本机配置继续/);
      assert.equal(await page.locator('#setup-confirm-existing').isVisible(), true);
    });
    await check('declining-existing-configuration-keeps-model-actions-locked', async () => {
      page.once('dialog', dialog => dialog.dismiss());
      await page.locator('#setup-confirm-existing').click();
      assert.equal(await page.locator('#generate-document').isDisabled(), true);
    });
    await check('explicit-existing-configuration-enables-but-never-submits-generation', async () => {
      page.once('dialog', dialog => dialog.accept());
      await page.locator('#setup-confirm-existing').click();
      await page.waitForFunction(() => !document.querySelector('#generate-document').disabled);
      assert.match(await page.locator('#generate-hint').innerText(), /调用.*模型.*计费/);
      assert.equal(await page.locator('body').getAttribute('data-phase'), 'draft');
      await page.reload();
      await page.waitForFunction(() => !document.querySelector('#generate-document').disabled);
      assert.deepEqual(modelRequests, []);
      assert.deepEqual(acceptanceRequests, []);
      assert.deepEqual(errors, []);
      await page.locator('#generate-document').click({ trial: true });
      await page.screenshot({ path: path.join(output, 'real-setup-generation-ready.png') });
      await page.setViewportSize({ width: 375, height: 812 });
      assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true);
      await page.locator('#generate-document').click({ trial: true });
      await page.screenshot({ path: path.join(output, 'real-setup-mobile-ready.png') });
      assert.deepEqual(modelRequests, [], 'Hit-testing must not submit generation');
    });
  } finally {
    fs.writeFileSync(path.join(output, 'report.json'), JSON.stringify({ results, errors, modelRequests, acceptanceRequests }, null, 2));
    console.log(JSON.stringify(results));
    await browser.close();
  }
})().catch(error => { console.error(String(error)); process.exitCode = 1; });
