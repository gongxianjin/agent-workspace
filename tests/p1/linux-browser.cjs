'use strict';
// Drive the actual remote Linux console through SSH; no model, browser download or consent.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const [playwrightArg, chromiumArg, output] = process.argv.slice(2);
const { chromium } = require(path.dirname(playwrightArg));
(async () => {
  const input = JSON.parse(fs.readFileSync(0, 'utf8'));
  const browser = await chromium.launch({ headless: true, executablePath: chromiumArg });
  const results = [], errors = [], posts = [];
  try {
    const context = await browser.newContext({ viewport: { width: 1440, height: 1000 } });
    const page = await context.newPage();
    page.on('pageerror', error => errors.push(error.name));
    page.on('request', request => { if (request.method() === 'POST') posts.push(new URL(request.url()).pathname); });
    async function check(name, run) {
      try { await run(); results.push({ name, passed: true }); }
      catch (error) { results.push({ name, passed: false, error: error.message.split('\n')[0] }); throw Error(name); }
    }
    await page.goto(input.url);
    await page.waitForFunction(() => document.body.dataset.connected === 'true');
    await check('actual-linux-mode-is-not-misrepresented-as-windows-or-test-ready', async () => {
      assert.match(await page.locator('#connection-status').innerText(), /Linux.*执行.*禁用/);
      assert.match(await page.locator('#prototype-notice').innerText(), /Linux.*未.*验证/);
      await page.locator('#setup-instructions > summary').click();
      assert.match(await page.locator('#setup-install').innerText(), /Linux/);
      await page.locator('#setup-instructions > summary').click();
      assert.equal(await page.locator('#setup-browser-prepare').isVisible(), false);
      assert.equal(await page.locator('#setup-confirm-existing').isVisible(), false);
    });
    await check('actual-project-create-save-switch-and-reload-persist', async () => {
      await page.locator('#new-project-name').fill('210浏览器项目一');
      await page.locator('#create-project').click();
      await page.waitForFunction(() => document.body.dataset.project?.length === 32);
      const first = await page.locator('body').getAttribute('data-project');
      await page.locator('#requirement').fill('仅保存需求，不调用模型。');
      await page.locator('#save-requirement').click();
      await page.waitForFunction(() => !document.querySelector('#save-requirement').disabled);
      await page.locator('#new-project-name').fill('210浏览器项目二');
      await page.locator('#create-project').click();
      await page.waitForFunction(id => document.body.dataset.project !== id, first);
      await page.locator('#project-select').selectOption(first);
      await page.waitForFunction(id => document.body.dataset.project === id, first);
      assert.equal(await page.locator('#requirement').inputValue(), '仅保存需求，不调用模型。');
      await page.reload();
      await page.waitForFunction(() => document.body.dataset.connected === 'true');
      assert.equal(await page.locator('body').getAttribute('data-project'), first);
      assert.equal(await page.locator('#requirement').inputValue(), '仅保存需求，不调用模型。');
      assert.equal(await page.locator('#project-select option').count(), input.projectCount + 2);
      assert.equal(await page.locator('#generate-document').isDisabled(), true);
      assert.match(await page.locator('#generate-hint').innerText(), /Linux.*执行.*验证/);
    });
    await check('visible-project-identity-follows-real-selection-without-demo-residue', async () => {
      const first = await page.locator('body').getAttribute('data-project');
      const options = await page.locator('#project-select option').evaluateAll(elements =>
        elements.map(element => ({ id: element.value, name: element.textContent })));
      const second = [...options].reverse().find(option => option.name.startsWith('210浏览器项目二 · '));
      for (const [id, name] of [[second.id, '210浏览器项目二'], [first, '210浏览器项目一']]) {
        await page.locator('#project-select').selectOption(id);
        await page.waitForFunction(value => document.body.dataset.project === value, id);
        assert.equal(await page.locator('.side-project strong').innerText(), name);
        assert.match(await page.locator('.side-project p').innerText(), new RegExp(id.slice(0, 8)));
        assert.equal((await page.locator('.breadcrumb').innerText()).includes(name), true);
        assert.equal((await page.locator('.project-badge').innerText()).trim(), name);
        assert.equal((await page.title()).includes(name), true);
      }
      await page.reload();
      await page.waitForFunction(() => document.body.dataset.connected === 'true');
      assert.equal(await page.locator('.side-project strong').innerText(), '210浏览器项目一');
    });
    await check('separate-preview-really-loads-through-second-ssh-forward', async () => {
      const preview = await page.context().newPage();
      try {
        await preview.goto(input.previewUrl);
        assert.equal(new URL(preview.url()).port, '8788');
        assert.equal(await preview.locator('#probe').innerText(), 'SSH separate preview');
      } finally { await preview.close(); }
      const frame = await page.evaluate(async url => {
        const iframe = document.createElement('iframe');
        iframe.id = 'ssh-preview-probe'; iframe.sandbox = 'allow-scripts'; iframe.src = url;
        document.body.append(iframe);
        return iframe.getAttribute('sandbox');
      }, input.previewUrl);
      assert.equal(frame, 'allow-scripts');
      await page.frameLocator('#ssh-preview-probe').locator('#probe').waitFor();
      assert.equal(await page.frameLocator('#ssh-preview-probe').locator('#probe').innerText(), 'SSH separate preview');
      await page.locator('#ssh-preview-probe').evaluate(element => element.remove());
    });
    await check('non-model-recheck-and-mobile-blockers-remain-honest', async () => {
      await page.locator('#setup-recheck').click();
      await page.waitForFunction(() => !document.querySelector('#setup-recheck').disabled);
      assert.equal(await page.locator('#generate-document').isDisabled(), true);
      assert.match(await page.locator('#setup-detail').innerText(), /Linux.*尚未验证/);
      await page.screenshot({ path: path.join(output, 'linux-console-desktop.png'), fullPage: true });
      await page.setViewportSize({ width: 375, height: 812 });
      assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true);
      await page.screenshot({ path: path.join(output, 'linux-console-mobile.png'), fullPage: true });
      assert.deepEqual(errors, []);
      assert.equal(posts.includes('/api/command'), true); // requirement save is a non-model command
      assert.equal(posts.includes('/api/setup/browser'), false);
      assert.equal(posts.includes('/api/setup/distribution'), false);
    });
  } finally {
    fs.writeFileSync(path.join(output, 'report.json'), JSON.stringify({ results, errors, posts,
      browserLocation: 'Windows client through two SSH forwards; not server-side Linux Chromium proof' }, null, 2));
    console.log(JSON.stringify({ results, errors }));
    await browser.close();
  }
})().catch(() => { console.error('Linux browser validation failed; see redacted report'); process.exitCode = 1; });
