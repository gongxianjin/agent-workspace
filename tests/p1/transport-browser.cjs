'use strict';
const fs = require('node:fs');
const path = require('node:path');
const assert = require('node:assert/strict');
const [launchFile, output, playwright, executablePath] = process.argv.slice(2);
const { chromium } = require(path.dirname(playwright));
(async () => {
  const { urls } = JSON.parse(fs.readFileSync(launchFile, 'utf8'));
  const browser = await chromium.launch({ headless: true, executablePath });
  const results = [];
  async function check(name, run) {
    try { await run(); results.push({ name, passed: true }); }
    catch (error) { results.push({ name, passed: false, error: String(error) }); }
  }
  try {
    await check('held-setup-read-does-not-block-project-sync-and-never-enables-execution', async () => {
      const context = await browser.newContext(); const page = await context.newPage();
      let release, entered = false, states = 0; const errors = [];
      page.on('pageerror', error => errors.push(String(error)));
      page.on('request', request => { if (new URL(request.url()).pathname === '/api/state') states++; });
      const gate = new Promise(resolve => { release = resolve; });
      await context.route('**/api/setup', async route => { entered = true; await gate; await route.continue().catch(() => {}); });
      try {
        await page.goto(urls[0]);
        await page.waitForFunction(() => document.body.dataset.connected === 'true', null, { timeout: 4000 });
        assert.equal(entered, true);
        assert.equal(await page.locator('#generate-document').isDisabled(), true);
        const project = await page.locator('body').getAttribute('data-project'); assert.equal(project.length, 32);
        const changed = await page.evaluate(async projectId => {
          const token = sessionStorage.getItem('relay-session');
          const headers = { Authorization: 'Bearer ' + token, 'Content-Type': 'application/json' };
          const state = (await (await fetch('/api/state', { headers })).json()).projects.find(item => item.id === projectId);
          const response = await fetch('/api/command', { method: 'POST', headers, body: JSON.stringify({ projectId,
            commandId: crypto.randomUUID(), expectedRevision: state.revision, kind: 'edit', payload: { requirement: '慢接入检测期间状态仍更新' } }) });
          return response.status;
        }, project);
        assert.equal(changed, 200);
        await page.waitForFunction(() => document.querySelector('#requirement').value === '慢接入检测期间状态仍更新', null, { timeout: 4000 });
        assert.ok(states >= 2); assert.equal(await page.locator('#generate-document').isDisabled(), true); assert.deepEqual(errors, []);
      } finally { release(); await context.close(); }
    });
    await check('storage-getter-failure-keeps-live-ui-usable-and-explicitly-warns-about-refresh', async () => {
      const context = await browser.newContext({ viewport: { width: 375, height: 812 } }); const page = await context.newPage(); const errors = [];
      page.on('pageerror', error => errors.push(String(error)));
      await context.addInitScript(() => Object.defineProperty(window, 'sessionStorage', { get() { throw new DOMException('blocked', 'SecurityError'); } }));
      try {
        await page.goto(urls[1]);
        await page.waitForFunction(() => document.body.dataset.connected === 'true', null, { timeout: 4000 });
        assert.match(await page.locator('#storage-warning').innerText(), /存储.*不可用|存储.*受限/);
        assert.match(await page.locator('#storage-warning').innerText(), /刷新/);
        const connection = await page.locator('#connection-status').boundingBox();
        const warning = await page.locator('#storage-warning').boundingBox();
        const toolbar = await page.locator('#my-projects').boundingBox();
        const progress = await page.locator('#current-progress').boundingBox();
        assert.ok(warning.y >= connection.y + connection.height, 'Storage warning must follow, not overlay, connection text');
        assert.ok(warning.y + warning.height <= toolbar.y + toolbar.height, 'Warning must stay inside the project panel');
        assert.ok(progress.y >= toolbar.y + toolbar.height, 'Progress must not overlap the warning');
        assert.ok(await page.locator('#storage-warning').evaluate(element => parseFloat(getComputedStyle(element).fontSize) >= 12), 'Storage limitations must be readable, not inherited 9px footnotes');
        await page.locator('#new-project-name').fill('仅标签内存项目'); await page.locator('#create-project').click();
        await page.waitForFunction(() => document.querySelector('#current-project-name').textContent === '仅标签内存项目' && document.body.dataset.uploading === 'false');
        await page.locator('#requirement').fill('存储不可用仍能明确保存'); await page.locator('#save-requirement').click();
        await page.waitForFunction(() => document.body.dataset.uploading === 'false' && document.querySelector('#requirement').value === '存储不可用仍能明确保存');
        assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true);
        const layout = () => page.evaluate(() => {
          const skip = document.querySelector('.skip'), rect = skip.getBoundingClientRect();
          return { scrollY, activeElement: document.activeElement?.id || document.activeElement?.tagName,
            skip: { text: skip.textContent, focused: document.activeElement === skip,
              position: getComputedStyle(skip).position, top: getComputedStyle(skip).top,
              x: rect.x, y: rect.y, width: rect.width, height: rect.height } };
        });
        const beforeScroll = await layout();
        await page.screenshot({ path: path.join(output, 'storage-unavailable-scrolled.png'), fullPage: true });
        await page.evaluate(() => window.scrollTo(0, 0));
        const atTop = await layout();
        fs.writeFileSync(path.join(output, 'storage-layout.json'), JSON.stringify({ beforeScroll, atTop }, null, 2));
        await page.screenshot({ path: path.join(output, 'storage-unavailable-mobile.png'), fullPage: true });
        await page.locator('.skip').focus();
        const focusedSkip = await page.locator('.skip').boundingBox();
        assert.ok(focusedSkip.y >= 0 && focusedSkip.y < 40, 'Keyboard skip link remains visible on focus');
        await page.keyboard.press('Enter');
        assert.equal(await page.locator('#requirement').evaluate(element => element === document.activeElement), true);
        await page.reload();
        await page.waitForFunction(() => /会话.*失效/.test(document.querySelector('#connection-status').textContent), null, { timeout: 4000 });
        assert.equal(await page.locator('#generate-document').isDisabled(), true); assert.deepEqual(errors, []);
      } finally { await context.close(); }
    });
    await check('telemetry-backoff-is-single-flight-and-auth-failure-cannot-be-woken-by-state-render', async () => {
      const page = await browser.newPage();
      try {
        await page.setContent('<div class="log-tabs"></div><div id="event-log"></div>');
        await page.clock.install();
        await page.clock.pauseAt(new Date());
        await page.addScriptTag({ path: path.join(__dirname, '../../live-session.js') });
        await page.addScriptTag({ path: path.join(__dirname, '../../live-output.js') });
        await page.evaluate(() => {
          const state = { id: 'a'.repeat(32), epoch: 1, activeAttempt: null };
          const node = (tag, classes = '', text = '') => Object.assign(document.createElement(tag), { className: classes, textContent: text });
          const fixture = { calls: 0, expired: false, state };
          fixture.view = RelayOutput.mount({ node, api: async () => {
            fixture.calls++;
            throw Object.assign(Error(fixture.expired ? 'expired' : 'offline'), { category: 'http', status: fixture.expired ? 403 : 500 });
          } });
          window.telemetryFault = fixture; fixture.view.setProject(state, true);
        });
        assert.equal(await page.evaluate(() => telemetryFault.calls), 1);
        await page.clock.runFor(2399); assert.equal(await page.evaluate(() => telemetryFault.calls), 1);
        await page.clock.runFor(1); assert.equal(await page.evaluate(() => telemetryFault.calls), 2);
        await page.clock.runFor(4799); assert.equal(await page.evaluate(() => telemetryFault.calls), 2);
        await page.evaluate(() => { telemetryFault.expired = true; });
        await page.clock.runFor(1); assert.equal(await page.evaluate(() => telemetryFault.calls), 3);
        await page.evaluate(() => { for (let i = 0; i < 10; i++) telemetryFault.view.setProject(telemetryFault.state, true); });
        await page.clock.runFor(60000); assert.equal(await page.evaluate(() => telemetryFault.calls), 3);
        assert.match(await page.locator('#claude-status').innerText(), /会话.*失效/);
        await page.evaluate(() => telemetryFault.view.stop());
      } finally { await page.close(); }
    });
    await check('authentication-expiry-stops-state-setup-and-telemetry-without-post-replay', async () => {
      const context = await browser.newContext(); const page = await context.newPage(); let reads = 0, writes = 0; const errors = [];
      page.on('pageerror', error => errors.push(String(error)));
      await context.route('**/api/**', async route => {
        const request = route.request(), pathname = new URL(request.url()).pathname;
        if (request.method() === 'POST') { if (pathname !== '/api/session') writes++; return route.continue(); }
        reads++; return route.fulfill({ status: 403, json: { error: 'Session expired or missing' } });
      });
      try {
        await page.goto(urls[2]);
        await page.waitForFunction(() => /会话.*失效/.test(document.querySelector('#connection-status').textContent), null, { timeout: 4000 });
        await page.waitForTimeout(200); const before = reads; await page.waitForTimeout(3000);
        assert.ok(before >= 1); assert.equal(reads, before); assert.equal(writes, 0);
        assert.equal(await page.locator('#create-project').isDisabled(), true);
        assert.equal(await page.locator('#setup-recheck').isDisabled(), true); assert.deepEqual(errors, []);
      } finally { await context.close(); }
    });
  } finally {
    await browser.close(); fs.writeFileSync(path.join(output, 'transport-results.json'), JSON.stringify(results, null, 2));
  }
  console.log(JSON.stringify(results)); process.exitCode = results.every(item => item.passed) ? 0 : 1;
})().catch(error => { console.error(error); process.exitCode = 1; });
