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
    await check('current-agent-task-and-bound-substages-visible-without-opening-output', async () => {
      const context = await browser.newContext({ viewport: { width: 375, height: 812 } });
      const page = await context.newPage(); const errors = [], telemetryReads = []; let writes = 0, stage = 'cli_running', foreign = false;
      page.on('pageerror', error => errors.push(String(error)));
      page.on('request', request => { if (request.method() === 'POST' && !request.url().endsWith('/api/session')) writes++; });
      await context.route('**/api/telemetry?*', async route => {
        const response = await route.fetch(); const value = await response.json();
        telemetryReads.push({ status: response.status(), error: value.error || null, hasScope: !!value.scope });
        if (response.status() !== 200 || !value.scope) { await route.fulfill({ response }); return; }
        const metadata = { ...value.metadata, status: 'running', stage, startedAt: value.serverNow - 18000,
          queuedAt: value.serverNow - 20000, lastEventAt: value.serverNow - 2000 };
        if (stage !== 'cli_running') metadata.cliEndedAt = value.serverNow - 10000;
        await route.fulfill({ status: 200, json: { ...value, available: true, sealed: false, metadata,
          scope: { ...value.scope, projectId: foreign ? 'f'.repeat(32) : value.scope.projectId },
          events: value.events.map(event => ({ ...event, at: value.serverNow - 2000 })) } });
      });
      try {
        await page.goto(urls[0]);
        await page.waitForFunction(() => document.body.dataset.connected === 'true');
        await page.waitForFunction(() => document.querySelector('#progress-agent')?.textContent.includes('CLI 正在运行'), null, { timeout: 4000 });
        assert.match(await page.locator('#progress-task').innerText(), /D2.*交互与存储.*已完成 1.*共 2/);
        assert.match(await page.locator('#progress-activity').innerText(), /写入.*已返回/);
        assert.match(await page.locator('#progress-timing').innerText(), /18 秒/);
        assert.equal(await page.locator('#claude-panel').isVisible(), false);
        assert.doesNotMatch(await page.locator('#progress-title').innerText(), /通过|已完成/);
        assert.equal(await page.locator('#progress-output').getAttribute('href'), '#claude-panel');
        await page.locator('#progress-output').click();
        assert.equal(await page.locator('#claude-panel').isVisible(), true);
        await page.locator('#tab-workflow').click();
        await page.evaluate(() => scrollTo(0, 0));
        assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true);
        await page.screenshot({ path: path.join(output, 'agent-progress-mobile.png'), fullPage: true });
        stage = 'host_validation';
        await page.waitForFunction(() => document.querySelector('#progress-agent').textContent.includes('宿主校验'), null, { timeout: 5000 });
        assert.match(await page.locator('#progress-timing').innerText(), /CLI.*8 秒/);
        stage = 'trusted_rerun';
        await page.waitForFunction(() => document.querySelector('#progress-agent').textContent.includes('可信检查器'), null, { timeout: 5000 });
        await page.setViewportSize({ width: 1440, height: 1000 }); await page.evaluate(() => scrollTo(0, 0));
        assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true);
        await page.screenshot({ path: path.join(output, 'agent-progress-desktop.png'), fullPage: true });
        foreign = true;
        await page.waitForFunction(() => !document.querySelector('#progress-agent').textContent.includes('可信检查器'), null, { timeout: 5000 });
        assert.doesNotMatch(await page.locator('#progress-activity').innerText(), /写入.*已返回/);
        assert.equal(writes, 0); assert.deepEqual(errors, []);
      } finally {
        await context.unrouteAll({ behavior: 'wait' });
        fs.writeFileSync(path.join(output, 'telemetry-reads.json'), JSON.stringify(telemetryReads, null, 2));
        await context.close();
      }
    });
    await check('safe-observation-clears-offline-errors-and-project-switch-without-private-text', async () => {
      const page = await browser.newPage();
      try {
        await page.setContent('<div class="log-tabs"></div><div id="event-log"></div>');
        await page.clock.install(); await page.clock.pauseAt(new Date());
        await page.addScriptTag({ path: path.join(__dirname, '../../live-output.js') });
        await page.evaluate(() => {
          const node = (tag, classes = '', text = '') => Object.assign(document.createElement(tag), { className: classes, textContent: text });
          const value = { pending: [], observations: [], state: { id: 'a'.repeat(32), epoch: 1, activeAttempt: { id: 'b'.repeat(32) } } };
          value.view = RelayOutput.mount({ node, api: () => new Promise((resolve, reject) => value.pending.push({ resolve, reject })), observed: page => value.observations.push(page) });
          window.observation = value; value.view.setProject(value.state, true);
          value.pending.shift().resolve({ available: true, serverNow: 30000, scope: { projectId: value.state.id, attemptId: 'b'.repeat(32), epoch: 1 },
            authority: {}, metadata: { status: 'running', role: 'developer', stage: 'cli_running' }, events: [{ kind: 'text', text: 'private-body-marker', seq: 1, at: 25000 }], nextCursor: 1 });
        });
        await page.evaluate(() => Promise.resolve());
        let observations = await page.evaluate(() => observation.observations);
        assert.ok(observations.at(-1)?.activity, 'A successful read must publish safe activity to the top panel');
        assert.equal(JSON.stringify(observations).includes('private-body-marker'), false);
        await page.evaluate(() => observation.view.setProject(observation.state, false));
        assert.equal(await page.evaluate(() => observation.observations.at(-1)), null);
        await page.evaluate(() => observation.view.setProject(observation.state, true));
        await page.evaluate(() => observation.pending.shift().reject(Error('synthetic failure')));
        await page.evaluate(() => Promise.resolve());
        assert.equal(await page.evaluate(() => observation.observations.at(-1)), null);
        await page.evaluate(() => {
          observation.view.setProject({ ...observation.state, id: 'f'.repeat(32) }, true);
          observation.view.stop();
        });
        assert.equal(await page.evaluate(() => observation.observations.at(-1)), null);
      } finally { await page.close(); }
    });
    await check('late-observation-cannot-return-after-project-switch-offline-or-stop', async () => {
      const page = await browser.newPage();
      try {
        await page.setContent('<div class="log-tabs"></div><div id="event-log"></div>');
        await page.clock.install(); await page.clock.pauseAt(new Date());
        await page.addScriptTag({ path: path.join(__dirname, '../../live-output.js') });
        await page.evaluate(() => {
          const node = (tag, classes = '', text = '') => Object.assign(document.createElement(tag), { className: classes, textContent: text });
          const value = { pending: [], observations: [], state: { id: 'a'.repeat(32), epoch: 1, activeAttempt: { id: 'b'.repeat(32) } } };
          value.view = RelayOutput.mount({ node, api: () => new Promise(resolve => value.pending.push(resolve)), observed: page => value.observations.push(page) });
          value.reply = id => ({ available: true, serverNow: 30000, scope: { projectId: id, attemptId: 'b'.repeat(32), epoch: 1 },
            metadata: { status: 'running', role: 'developer', stage: 'cli_running', privateText: 'private-marker' }, authority: {},
            events: [{ kind: 'text', text: 'private-marker', seq: 1, at: 25000 }], nextCursor: 1 });
          window.lateObservation = value;
          value.view.setProject(value.state, true);
          value.view.setProject({ ...value.state, id: 'c'.repeat(32) }, true);
          value.pending.shift()(value.reply(value.state.id));
        });
        await page.evaluate(() => Promise.resolve());
        assert.equal(await page.evaluate(() => lateObservation.observations.at(-1)), null);
        assert.equal(await page.locator('#claude-output').innerText(), '');
        await page.evaluate(() => lateObservation.pending.shift()(lateObservation.reply('c'.repeat(32))));
        await page.evaluate(() => Promise.resolve());
        assert.equal(await page.evaluate(() => lateObservation.observations.at(-1).scope.projectId), 'c'.repeat(32));
        assert.equal(JSON.stringify(await page.evaluate(() => lateObservation.observations)).includes('private-marker'), false);
        for (const end of ['offline', 'stop']) {
          await page.evaluate(end => {
            const value = lateObservation;
            value.state = { ...value.state, id: end === 'stop' ? 'e'.repeat(32) : 'd'.repeat(32) };
            value.view.setProject(value.state, true);
            if (end === 'stop') value.view.stop(); else value.view.setProject(value.state, false);
            value.pending.shift()(value.reply(value.state.id));
          }, end);
          await page.evaluate(() => Promise.resolve());
          assert.equal(await page.evaluate(() => lateObservation.observations.at(-1)), null);
          assert.equal(await page.locator('#claude-output').innerText(), '');
        }
      } finally { await page.close(); }
    });
  } finally { await browser.close(); }
  fs.writeFileSync(path.join(output, 'agent-results.json'), JSON.stringify(results, null, 2));
  console.log(JSON.stringify(results)); process.exitCode = results.every(row => row.passed) ? 0 : 1;
})().catch(error => { console.error(error); process.exitCode = 1; });
