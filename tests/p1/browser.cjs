'use strict';
// Real HTTP UI tests, with workers disabled explicitly; never fabricates model completion.
const fs = require('node:fs');
const path = require('node:path');
const assert = require('node:assert/strict');
const [launchFile, output, playwrightArg, chromiumArg] = process.argv.slice(2);
if (!playwrightArg || !chromiumArg || !path.isAbsolute(playwrightArg) || !path.isAbsolute(chromiumArg)) {
  throw Error('Use run_browser.py with trusted explicit browser dependencies');
}
const { chromium } = require(path.dirname(playwrightArg));
(async () => {
  const { url } = JSON.parse(fs.readFileSync(launchFile, 'utf8'));
  const browser = await chromium.launch({ headless: true, executablePath: chromiumArg });
  const results = [], errors = [];
  try {
    const context = await browser.newContext({ viewport: { width: 375, height: 812 }, acceptDownloads: true });
    const page = await context.newPage();
    page.on('pageerror', error => errors.push(String(error)));
    async function check(name, run) {
      try { await run(); results.push({ name, passed: true }); }
      catch (error) {
        results.push({ name, passed: false, error: String(error), pageErrors: [...errors],
          connection: await page.locator('#connection-status').textContent().catch(() => null),
          setup: await page.locator('#setup-status').textContent().catch(() => null) });
        throw error;
      }
    }
    await page.goto(url);
    await check('bare-address-explains-missing-session-without-unlocking-actions', async () => {
      const fresh = await browser.newContext();
      try {
        const unauthenticated = await fresh.newPage();
        await unauthenticated.goto(new URL('/', url).href);
        await unauthenticated.waitForFunction(() => document.body.dataset.connected === 'false');
        assert.equal(await unauthenticated.locator('#generate-document').isDisabled(), true);
        assert.match(await unauthenticated.locator('#generate-hint').innerText(), /双击.*start-workbench\.cmd/);
        assert.match(await unauthenticated.locator('#generate-hint').innerText(), /会话|连接/);
      } finally { await fresh.close(); }
    });
    await check('live-mode-no-simulation-fallback', async () => {
      await page.waitForFunction(() => document.body.dataset.mode === 'live');
      assert.match(await page.locator('#prototype-notice').innerText(), /真实/);
      await page.locator('#new-project-name').fill('浏览器独立测试');
      await page.locator('#create-project').click();
      await page.waitForFunction(() => document.body.dataset.project?.length === 32);
      for (const role of ['architect', 'developer', 'tester']) assert.equal(await page.locator('#role-' + role).inputValue(), 'Claude Code');
      assert.equal(await page.locator('#retest').count(), 1);
      assert.equal(await page.locator('#retest').isDisabled(), true);
      for (const id of ['develop-complete', 'test-pass', 'test-fail', 'fix-complete']) assert.equal(await page.locator('#' + id).isVisible(), false);
      assert.equal(await page.locator('#start').isDisabled(), true);
    });
    await check('onboarding-is-visible-and-explicit-without-running-detection', async () => {
      assert.equal(await page.locator('#setup-panel').isVisible(), true);
      await page.locator('#setup-instructions > summary').click();
      assert.match(await page.locator('#setup-panel').innerText(), /未验证/);
      assert.equal(await page.locator('#setup-recheck').isEnabled(), true);
      assert.match(await page.locator('#setup-install').getAttribute('href'), /^https:\/\//);
      assert.match(await page.locator('#setup-panel').innerText(), /claude auth login/);
      assert.match(await page.locator('#setup-panel').innerText(), /关闭.*不.*停止/);
      assert.equal(await page.locator('#shutdown-workbench').count(), 1);
    });
    await check('generation-guidance-explains-project-and-setup-gates-without-model-calls', async () => {
      const baseline = await page.evaluate(async () => (await fetch('/api/setup', {
        headers: { Authorization: 'Bearer ' + sessionStorage.getItem('relay-session') }
      })).json());
      let setupState = baseline;
      await context.route('**/api/setup', route => route.fulfill({ json: setupState }));
      try {
        for (const [patch, expected] of [
          [{ client: 'unchecked', compatibility: 'unchecked', dependencies: 'unchecked', existingConfirmed: false }, /重新检测/],
          [{ client: 'missing', existingConfirmed: false }, /Claude.*安装|安装.*Claude/],
          [{ compatibility: 'unsupported', existingConfirmed: false }, /版本|参数/],
          [{ dependencies: 'unhealthy', existingConfirmed: false }, /依赖|自检/],
          [{ login: 'logged_out', existingConfirmed: false }, /claude auth login/],
          [{ login: 'unknown', existingConfirmed: false }, /使用已有本机配置继续/],
          [{ distributionTerms: { required: true, accepted: false, text: 'Synthetic conditions', sha256: 'a'.repeat(64) } }, /条款/],
          [{ browser: { status: 'missing', version: '145.0.7632.6' } }, /准备测试浏览器/]
        ]) {
          setupState = { ...baseline, ...patch, canExecute: false };
          await page.waitForFunction(pattern => new RegExp(pattern).test(document.querySelector('#generate-hint')?.textContent || ''), expected.source);
          assert.equal(await page.locator('#generate-document').isDisabled(), true);
          assert.match(await page.locator('#generate-document').getAttribute('aria-describedby'), /generate-hint/);
        }
        setupState = baseline;
        await page.waitForFunction(() => !document.querySelector('#generate-document').disabled);
        assert.match(await page.locator('#generate-hint').innerText(), /调用.*模型.*计费/);
        await page.locator('#generate-document').click({ trial: true });
        await context.route('**/api/state', route => route.fulfill({ json: {
          projects: [], mode: 'live', workers: false, reason: 'synthetic empty project list', clients: {}
        } }));
        try {
          await page.waitForFunction(() => document.body.dataset.project === '');
          assert.equal(await page.locator('#generate-document').isDisabled(), true);
          assert.match(await page.locator('#generate-hint').innerText(), /新建.*项目/);
        } finally { await context.unroute('**/api/state'); }
        await page.waitForFunction(() => document.body.dataset.project?.length === 32);
      } finally { await context.unroute('**/api/setup'); }
    });
    await check('distribution-terms-are-literal-and-need-explicit-current-document-consent', async () => {
      const text = 'Synthetic Windows conditions <img src=x onerror="window.termsPwned=true">; not a model or license approval.\n' + ('https://example.invalid/' + 'long-unbroken-fixture'.repeat(80) + '\n').repeat(4);
      let accepted = false, accepts = 0;
      const state = () => ({ client: 'unchecked', version: null, compatibility: 'unchecked', login: 'unknown', dependencies: 'unchecked',
        canExecute: false, message: 'synthetic terms boundary; no detection or download',
        distributionTerms: { required: true, accepted, sha256: 'a'.repeat(64), text } });
      await context.route('**/api/setup', route => route.fulfill({ json: state() }));
      await context.route('**/api/setup/distribution', async route => {
        assert.deepEqual(route.request().postDataJSON(), { confirmed: true, sha256: 'a'.repeat(64) });
        accepts++; accepted = true;
        await route.fulfill({ json: state() });
      });
      try {
        await page.waitForFunction(() => document.querySelector('#setup-distribution-accept')?.offsetParent !== null && !!document.querySelector('#setup-distribution-accept'), null, { timeout: 5000 });
        assert.equal(accepts, 0);
        assert.equal(await page.locator('#setup-recheck').isDisabled(), true);
        assert.equal(await page.locator('#shutdown-workbench').isEnabled(), true, 'Declining must not trap the user in the running host');
        assert.equal(await page.locator('#setup-distribution-text').textContent(), text);
        assert.equal(await page.locator('#setup-distribution-text img').count(), 0);
        assert.equal(await page.evaluate(() => !!window.termsPwned), false);
        assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1), true, 'Long distribution terms must wrap without making the mobile page overflow');
        const termsBox = await page.locator('#setup-distribution-text').boundingBox();
        assert.ok(termsBox.height <= 360, 'Long terms must scroll within a bounded reading area');
        page.once('dialog', dialog => dialog.dismiss());
        await page.locator('#setup-distribution-accept').click();
        assert.equal(accepts, 0);
        page.once('dialog', dialog => dialog.accept());
        await page.locator('#setup-distribution-accept').click();
        await page.waitForFunction(() => document.querySelector('#setup-distribution-accept').hidden);
        assert.equal(accepts, 1);
        assert.equal(await page.locator('#setup-recheck').isEnabled(), true);
        assert.equal(await page.locator('#generate-document').isDisabled(), true, 'Consent alone cannot unlock model actions');
        await page.reload();
        await page.waitForFunction(() => document.body.dataset.connected === 'true');
        assert.equal(accepts, 1, 'Reloading cannot submit acceptance');
      } finally {
        await context.unroute('**/api/setup');
        await context.unroute('**/api/setup/distribution');
      }
      await page.waitForFunction(() => !document.querySelector('#setup-recheck').disabled, null, { timeout: 5000 });
    });
    await check('first-use-browser-requires-explicit-consent-and-shows-download-progress', async () => {
      const before = await page.locator('body').getAttribute('data-phase');
      let starts = 0, prepared = false, failed = false, releaseRetry, retryStarted;
      const retryResponse = new Promise(resolve => { releaseRetry = resolve; });
      const retryRequest = new Promise(resolve => { retryStarted = resolve; });
      await context.route('**/api/setup', async route => route.fulfill({ json: {
        client: 'missing', version: null, compatibility: 'unchecked', login: 'unknown', dependencies: 'missing', canExecute: false,
        message: 'synthetic first-use boundary; no download or model',
        browser: { status: failed ? 'failed' : prepared ? 'downloading' : 'missing', version: '145.0.7632.6', downloadedBytes: prepared ? 1048576 : 0,
          stage: 'download', failureCode: failed ? 'tls_error' : null, message: failed ? 'TLS连接或证书验证失败；未自动重试。' : '',
          totalBytes: 181196793, source: 'https://cdn.playwright.dev/builds/cft/145.0.7632.6/win64/chrome-win64.zip', termsUrl: 'https://www.google.com/chrome/terms/' }
      } }));
      await context.route('**/api/setup/browser', async route => {
        assert.deepEqual(route.request().postDataJSON(), { confirmed: true });
        starts++; prepared = true; failed = false;
        if (starts === 2) { retryStarted(); await retryResponse; }
        await route.fulfill({ json: { client: 'missing', compatibility: 'unchecked', login: 'unknown', dependencies: 'missing', canExecute: false,
          message: 'synthetic first-use download started', browser: { status: 'downloading', version: '145.0.7632.6', downloadedBytes: starts === 2 ? 2097152 : 1048576,
            totalBytes: 181196793, source: 'https://cdn.playwright.dev/builds/cft/145.0.7632.6/win64/chrome-win64.zip', termsUrl: 'https://www.google.com/chrome/terms/' } } });
      });
      try {
        await page.waitForFunction(() => { const button = document.querySelector('#setup-browser-prepare'); return button && !button.hidden; }, null, { timeout: 5000 });
        assert.equal(starts, 0, 'Cached polling must not download automatically');
        assert.match(await page.locator('#setup-browser-status').innerText(), /首次|未准备/);
        assert.equal(await page.locator('#setup-browser-terms').getAttribute('href'), 'https://www.google.com/chrome/terms/');
        page.once('dialog', dialog => dialog.dismiss());
        await page.locator('#setup-browser-prepare').click();
        assert.equal(starts, 0, 'Declining the terms/download confirmation must have no side effect');
        page.once('dialog', dialog => dialog.accept());
        await page.locator('#setup-browser-prepare').click();
        await page.waitForFunction(() => document.querySelector('#setup-browser-status')?.textContent.includes('1.0 MiB'));
        assert.equal(starts, 1);
        assert.equal(await page.locator('#setup-browser-prepare').isDisabled(), true);
        assert.equal(await page.locator('#generate-document').isDisabled(), true);
        assert.equal(await page.locator('body').getAttribute('data-phase'), before);
        failed = true;
        await page.waitForFunction(() => document.querySelector('#setup-browser-status')?.textContent.includes('TLS'));
        assert.match(await page.locator('#setup-browser-status').innerText(), /TLS.*未自动重试/);
        assert.equal(await page.locator('#setup-browser-prepare').isEnabled(), true);
        assert.match(await page.locator('#setup-browser-prepare').innerText(), /明确重试/);
        assert.equal(starts, 1, 'Polling a failed download cannot silently retry');
        await page.reload();
        await page.waitForFunction(() => document.querySelector('#setup-browser-status')?.textContent.includes('TLS'));
        assert.equal(starts, 1, 'Reloading a failed download cannot silently retry');
        assert.equal(await page.locator('#generate-document').isDisabled(), true);
        page.once('dialog', dialog => dialog.dismiss());
        await page.locator('#setup-browser-prepare').click();
        assert.equal(starts, 1);
        page.once('dialog', dialog => dialog.accept());
        await page.locator('#setup-browser-prepare').click();
        await page.waitForFunction(() => document.querySelector('#setup-browser-prepare')?.disabled);
        await retryRequest;
        assert.equal(starts, 2, 'Only another explicit confirmation can start the next download');
        assert.match(await page.locator('#setup-browser-status').innerText(), /TLS/, 'A pending retry is not a completed response');
        assert.equal(await page.locator('#generate-document').isDisabled(), true);
        releaseRetry();
        await page.waitForFunction(() => {
          const text = document.querySelector('#setup-browser-status')?.textContent || '';
          return text.includes('正在下载') && text.includes('2.0 MiB') && !text.includes('TLS');
        });
        assert.equal(await page.locator('#setup-browser-prepare').isDisabled(), true);
        assert.equal(starts, 2, 'Rendering the retry response must not start another request');
        assert.doesNotMatch(await page.locator('#setup-browser-status').innerText(), /TLS/);
      } finally {
        releaseRetry();
        await context.unroute('**/api/setup');
        await context.unroute('**/api/setup/browser');
      }
      await page.waitForFunction(() => document.querySelector('#setup-browser-prepare')?.hidden, null, { timeout: 5000 });
    });
    await check('live-output-increments-without-completion-and-is-literal', async () => {
      await page.locator('#tab-claude').click();
      await page.waitForFunction(() => document.querySelector('#claude-status')?.textContent.includes('尚无'));
      const project = await page.locator('body').getAttribute('data-project');
      const scope = { projectId: project, attemptId: 'b'.repeat(32), epoch: 1, runId: null, inputHash: 'c'.repeat(64) };
      const clock = Date.now();
      let step = 0;
      await context.route('**/api/telemetry?**', async route => {
        const query = new URL(route.request().url()).searchParams;
        const cursor = Number(query.get('cursor') || 0);
        const events = [{ kind: 'text', text: '<img src=x onerror="window.livePwned=true"> synthetic public', seq: 1, at: clock }];
        if (step > 0) events.push({ kind: 'text', text: 'second synthetic increment', seq: 2, at: clock + 10000 });
        await route.fulfill({ json: { scope, events: events.filter(e => e.seq > cursor), nextCursor: events.length, hasMore: false, gap: false, sealed: false, available: true,
          serverNow: clock + step * 40000, metadata: { role: 'architect', kind: 'generate', status: step === 2 ? undefined : 'running', queuedAt: clock-4000, startedAt: clock-2000, waitStartedAt: clock-1900, timeoutAt: clock+120000, pid: 123, stage: 'cli_running', lastEventAt: events[events.length - 1].at },
          authority: { phase: 'designing', epoch: 1, revision: 1, leaseUnknown: false, recoveryRequired: false, activeAttemptId: scope.attemptId } } });
      });
      await page.waitForFunction(() => document.querySelector('#claude-output')?.textContent.includes('synthetic public'));
      assert.equal(await page.locator('#claude-output img').count(), 0);
      assert.equal(await page.evaluate(() => !!window.livePwned), false);
      assert.match(await page.locator('#claude-timing').innerText(), /PID 123/);
      assert.match(await page.locator('#claude-timing').innerText(), /不是.*预计/);
      assert.match(await page.locator('#claude-status').innerText(), /CLI 正在运行/);
      assert.match(await page.locator('#claude-timing').innerText(), /实际超时上限/);
      await page.locator('#claude-follow').click();
      assert.equal(await page.locator('#claude-follow').getAttribute('aria-pressed'), 'false');
      step = 1;
      await page.waitForFunction(() => document.querySelector('#claude-output')?.textContent.includes('second synthetic increment'));
      assert.match(await page.locator('#claude-timing').innerText(), /无新可展示事件/);
      step = 2; // Deliberately malformed API metadata: missing status must not claim a sealed attempt.
      await page.waitForFunction(() => document.querySelector('#claude-status').textContent.includes('状态未知'), null, { timeout: 5000 });
      assert.doesNotMatch(await page.locator('#claude-status').innerText(), /已封存/);
      assert.doesNotMatch(await page.locator('#claude-timing').innerText(), /距 CLI 实际超时上限/);
      assert.equal(await page.locator('#claude-output').evaluate(el => [...el.children].filter(n => n.textContent.includes('synthetic public')).length), 1);
      assert.equal(await page.locator('body').getAttribute('data-phase'), 'draft', 'Telemetry cannot change authoritative page state');
      await page.locator('#claude-follow').click();
      assert.equal(await page.locator('#claude-follow').getAttribute('aria-pressed'), 'true');
      await context.unroute('**/api/telemetry?**');
      await page.reload();
      await page.waitForFunction(() => document.body.dataset.connected === 'true');
    });
    await check('real-upload-xss-reload-and-cas-state', async () => {
      await page.locator('#document-file').setInputFiles({ name: 'reviewed.md', mimeType: 'text/plain', buffer: Buffer.from('# 人工确认\n<img src=x onerror="window.pwned=true">') });
      await page.waitForFunction(() => document.body.dataset.phase === 'uploaded');
      assert.match(await page.locator('#upload-preview').textContent(), /<img/);
      assert.equal(await page.locator('#upload-preview img').count(), 0);
      assert.equal(await page.evaluate(() => !!window.pwned), false);
      await page.reload();
      await page.waitForFunction(() => document.body.dataset.phase === 'uploaded');
      assert.equal(await page.locator('#start').isDisabled(), true);
      assert.equal(await page.locator('#split-tasks').isDisabled(), false);
    });
    await check('cached-setup-changes-in-another-page-lock-and-unlock-model-actions', async () => {
      const opened = context.waitForEvent('page');
      await page.evaluate(() => window.open(location.href, '_blank'));
      const other = await opened;
      try {
        await other.waitForFunction(() => document.body.dataset.connected === 'true', null, { timeout: 5000 });
        assert.equal(await page.locator('#split-tasks').isEnabled(), true);
        await other.locator('#setup-recheck').click();
        await other.waitForFunction(() => document.querySelector('#split-tasks').disabled);
        await page.waitForFunction(() => document.querySelector('#split-tasks').disabled, null, { timeout: 5000 });
        assert.equal(await page.locator('#generate-document').isDisabled(), true);
        assert.equal(await page.locator('body').getAttribute('data-phase'), 'uploaded');
        await other.locator('#setup-recheck').click();
        await page.waitForFunction(() => !document.querySelector('#split-tasks').disabled, null, { timeout: 5000 });
      } finally { await other.close(); }
    });
    await check('upload-read-locks-dispatch-and-failure-keeps-old-document', async () => {
      await page.evaluate(() => {
        const original = File.prototype.arrayBuffer;
        File.prototype.arrayBuffer = function () {
          if (!this.name.startsWith('slow-')) return original.call(this);
          return new Promise((resolve, reject) => { window.releaseFile = () => original.call(this).then(resolve, reject); window.failFile = () => reject(Error('fixture read failure')); });
        };
      });
      const previous = await page.locator('#upload-preview').textContent();
      await page.locator('#document-file').setInputFiles({ name: 'slow-review.md', mimeType: 'text/plain', buffer: Buffer.from('# Must not replace') });
      await page.waitForFunction(() => document.body.dataset.uploading === 'true', { timeout: 3000 });
      for (const id of ['split-tasks', 'generate-document', 'start', 'project-select']) assert.equal(await page.locator('#' + id).isDisabled(), true);
      assert.match(await page.locator('#generate-hint').innerText(), /保存|读取/);
      await page.evaluate(() => window.failFile());
      await page.waitForFunction(() => document.body.dataset.uploading === 'false');
      assert.equal(await page.locator('#upload-preview').textContent(), previous);
      assert.match(await page.locator('#upload-error').innerText(), /fixture read failure/);
    });
    await check('log-tabs-keyboard-navigation', async () => {
      await page.locator('#tab-workflow').focus();
      await page.keyboard.press('ArrowRight');
      assert.equal(await page.locator('#tab-reason').getAttribute('aria-selected'), 'true');
      assert.equal(await page.locator('#tab-reason').getAttribute('tabindex'), '0');
      await page.keyboard.press('ArrowRight');
      assert.equal(await page.locator('#tab-claude').getAttribute('aria-selected'), 'true');
      assert.equal(await page.locator('#claude-panel').isVisible(), true);
      await page.keyboard.press('Home');
      assert.equal(await page.locator('#tab-workflow').getAttribute('aria-selected'), 'true');
      await page.keyboard.press('End');
      assert.equal(await page.locator('#tab-claude').getAttribute('aria-selected'), 'true');
      await page.keyboard.press('ArrowRight');
      assert.equal(await page.locator('#tab-workflow').getAttribute('aria-selected'), 'true');
    });
    await check('explicit-retry-and-queued-stop-use-normal-api-without-automatic-models', async () => {
      const originalProject = await page.locator('body').getAttribute('data-project');
      const options = await page.locator('#project-select option').evaluateAll(items => items.map(item => ({ id: item.value, name: item.textContent })));
      const fixture = options.find(item => item.name.startsWith('仅测试：可安全显式重试 · '));
      assert.ok(fixture);
      await page.locator('#project-select').selectOption(fixture.id);
      await page.waitForFunction(() => document.body.dataset.phase === 'blocked');
      assert.equal(await page.locator('#retry').count(), 1, 'A safely stopped failed attempt must expose an explicit retry');
      assert.equal(await page.locator('#retry').isEnabled(), true);
      page.once('dialog', dialog => dialog.dismiss());
      await page.locator('#retry').click();
      assert.equal(await page.locator('body').getAttribute('data-phase'), 'blocked');
      page.once('dialog', dialog => dialog.accept());
      await page.locator('#retry').click();
      await page.waitForFunction(() => document.body.dataset.phase === 'designing' && document.body.dataset.uploading === 'false');
      assert.equal(await page.locator('#retry').isVisible(), false);
      await page.locator('#stop').click();
      await page.waitForFunction(() => document.body.dataset.phase === 'stopped' && document.body.dataset.uploading === 'false');
      await page.locator('#project-select').selectOption(originalProject);
      await page.waitForFunction(() => document.body.dataset.phase === 'uploaded');
    });
    await check('mobile-layout-no-page-errors', async () => {
      assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true);
      assert.deepEqual(errors, []);
      await page.screenshot({ path: path.join(output, 'live-mobile.png'), fullPage: true });
    });
    await check('backend-disconnection-locks-without-static-state', async () => {
      await context.route('**/api/**', route => route.abort());
      await page.waitForFunction(() => document.body.dataset.connected === 'false', { timeout: 10000 });
      assert.equal(await page.locator('#split-tasks').isDisabled(), true);
      assert.equal(await page.locator('#generate-document').isDisabled(), true);
      assert.match(await page.locator('#connection-status').innerText(), /断开|失败/);
      assert.notEqual(await page.locator('body').getAttribute('data-mode'), 'static');
    });
    await check('output-polling-resumes-after-same-attempt-reconnect', async () => {
      await context.unroute('**/api/**');
      const project = await page.locator('body').getAttribute('data-project');
      await context.route('**/api/telemetry?**', route => route.fulfill({ json: {
        scope: { projectId: project, attemptId: 'd'.repeat(32), epoch: 1, runId: null, inputHash: 'e'.repeat(64) },
        events: [{ kind: 'text', text: 'synthetic-reconnected-output', seq: 1, at: Date.now() }],
        nextCursor: 1, hasMore: false, gap: false, sealed: false, available: true, serverNow: Date.now(),
        metadata: { role: 'architect', status: 'running', queuedAt: Date.now()-4000, startedAt: Date.now()-2000, pid: 124, stage: 'cli_running' },
        authority: { phase: 'designing', epoch: 1, revision: 1, leaseUnknown: false, recoveryRequired: false, activeAttemptId: 'd'.repeat(32) }
      } }));
      await page.waitForFunction(() => document.body.dataset.connected === 'true', { timeout: 10000 });
      await page.locator('#tab-claude').click();
      await page.waitForFunction(() => document.querySelector('#claude-output')?.textContent.includes('synthetic-reconnected-output'), { timeout: 5000 });
      assert.equal(await page.locator('body').getAttribute('data-phase'), 'uploaded');
      await context.unroute('**/api/telemetry?**');
    });
    await check('late-output-is-isolated-by-project-attempt-and-epoch', async () => {
      const isolated = await context.newPage();
      try {
        await isolated.setContent('<div class="log-tabs"></div><div id="event-log"></div>');
        await isolated.addScriptTag({ path: path.join(__dirname, '../../live-output.js') });
        await isolated.evaluate(() => {
          const pending = [];
          const node = (tag, classes = '', text = '') => Object.assign(document.createElement(tag), { className: classes, textContent: text });
          const view = RelayOutput.mount({ node, api: url => new Promise(resolve => pending.push({ url, resolve })) });
          const state = (id, epoch, attempt) => ({ id, epoch, activeAttempt: { id: attempt, status: 'running', createdAt: 1 } });
          const reply = (index, id, epoch, attempt, text) => pending[index].resolve({
            scope: { projectId: id, epoch, attemptId: attempt, runId: null, inputHash: 'c'.repeat(64) },
            events: [{ kind: 'text', text, seq: 1, at: 2000 }], nextCursor: 1,
            hasMore: false, gap: false, sealed: false, available: true, serverNow: 3000,
            metadata: { role: 'architect', kind: 'generate', status: 'running', queuedAt: 1000, startedAt: 1500, pid: 123, stage: 'cli_running', lastEventAt: 2000 },
            authority: { phase: 'designing', epoch, revision: 1, leaseUnknown: false, recoveryRequired: false, activeAttemptId: attempt }
          });
          window.syntheticOutput = { view, state, reply, pending };
          view.setProject(state('a'.repeat(32), 1, 'b'.repeat(32)), true);
          view.setProject(state('d'.repeat(32), 1, 'e'.repeat(32)), true);
          reply(1, 'd'.repeat(32), 1, 'e'.repeat(32), 'current-project-output');
          reply(0, 'a'.repeat(32), 1, 'b'.repeat(32), 'stale-project-output');
        });
        await isolated.waitForFunction(() => document.querySelector('#claude-output').textContent.includes('current-project-output'));
        assert.doesNotMatch(await isolated.locator('#claude-output').innerText(), /stale-project/);
        await isolated.evaluate(() => {
          const { view, state, reply } = syntheticOutput;
          view.setProject(state('d'.repeat(32), 2, 'f'.repeat(32)), true);
          view.setProject(state('d'.repeat(32), 3, '1'.repeat(32)), true);
          reply(3, 'd'.repeat(32), 3, '1'.repeat(32), 'current-epoch-output');
          reply(2, 'd'.repeat(32), 2, 'f'.repeat(32), 'stale-epoch-output');
        });
        await isolated.waitForFunction(() => document.querySelector('#claude-output').textContent.includes('current-epoch-output'));
        assert.doesNotMatch(await isolated.locator('#claude-output').innerText(), /stale|current-project/);
        await isolated.evaluate(() => syntheticOutput.view.stop());
      } finally { await isolated.close(); }
    });
    await check('output-distinguishes-host-validation-frozen-cli-time-and-terminal', async () => {
      const isolated = await context.newPage();
      try {
        await isolated.setContent('<div class="log-tabs"></div><div id="event-log"></div>');
        await isolated.addScriptTag({ path: path.join(__dirname, '../../live-output.js') });
        await isolated.evaluate(() => {
          const node = (tag, classes = '', text = '') => Object.assign(document.createElement(tag), { className: classes, textContent: text });
          const state = { id: 'a'.repeat(32), epoch: 1, activeAttempt: { id: 'b'.repeat(32), status: 'running', createdAt: 1 } };
          const fixture = { now: 20000, stage: 'host_validation', status: 'running', sealed: false };
          const view = RelayOutput.mount({ node, api: async () => ({
            scope: { projectId: state.id, attemptId: state.activeAttempt.id, epoch: 1, runId: null, inputHash: 'c'.repeat(64) },
            events: [{ kind: 'tool', tool: 'run_check', phase: 'started', seq: 1, at: 8000 }], nextCursor: 1,
            hasMore: false, gap: false, sealed: fixture.sealed, available: true, serverNow: fixture.now,
            metadata: { role: 'tester', kind: 'test', status: fixture.status, stage: fixture.stage,
              queuedAt: 1000, startedAt: 4000, cliEndedAt: 9000, waitStartedAt: 4100, timeoutAt: 120000, pid: 123, lastEventAt: 8000 },
            authority: { phase: fixture.sealed ? 'awaiting_acceptance' : 'testing', epoch: 1, revision: 1,
              leaseUnknown: false, recoveryRequired: false, activeAttemptId: fixture.sealed ? null : state.activeAttempt.id }
          }) });
          window.frozenOutput = { fixture, view, state };
          view.setProject(state, true);
        });
        await isolated.waitForFunction(() => document.querySelector('#claude-status').textContent.includes('宿主校验'));
        assert.match(await isolated.locator('#claude-timing').innerText(), /排队／准备 3 秒.*CLI 已运行 5 秒/);
        assert.match(await isolated.locator('#claude-timing').innerText(), /暂无公开文本/);
        assert.doesNotMatch(await isolated.locator('#claude-timing').innerText(), /距 CLI|无新可展示事件/);
        await isolated.evaluate(() => {
          const { fixture, view, state } = frozenOutput;
          Object.assign(fixture, { now: 60000, stage: 'trusted_rerun' });
          view.setProject(state, false); view.setProject(state, true);
        });
        await isolated.waitForFunction(() => document.querySelector('#claude-status').textContent.includes('可信检查器'));
        assert.match(await isolated.locator('#claude-timing').innerText(), /CLI 已运行 5 秒/);
        await isolated.evaluate(() => {
          const { fixture, view, state } = frozenOutput;
          Object.assign(fixture, { now: 100000, status: 'complete', sealed: true });
          view.setProject(state, false); view.setProject(state, true);
        });
        await isolated.waitForFunction(() => document.querySelector('#claude-status').textContent.includes('awaiting_acceptance'));
        assert.match(await isolated.locator('#claude-timing').innerText(), /CLI 已运行 5 秒/);
        assert.doesNotMatch(await isolated.locator('#claude-timing').innerText(), /距 CLI|无新可展示事件/);
        assert.equal(await isolated.locator('#claude-output .host-activity').count(), 1, 'Repeated cursor pages cannot duplicate tool rows');
        await isolated.evaluate(() => frozenOutput.view.stop());
      } finally { await isolated.close(); }
    });
    await check('output-retention-is-bounded-and-manual-reading-is-not-overridden', async () => {
      const isolated = await context.newPage();
      try {
        await isolated.setContent('<div class="log-tabs"></div><div id="event-log"></div>');
        await isolated.addStyleTag({ path: path.join(__dirname, '../../styles.css') });
        await isolated.addScriptTag({ path: path.join(__dirname, '../../live-output.js') });
        await isolated.evaluate(() => {
          const node = (tag, classes = '', text = '') => Object.assign(document.createElement(tag), { className: classes, textContent: text });
          const state = { id: 'a'.repeat(32), epoch: 1, activeAttempt: { id: 'b'.repeat(32), status: 'running', createdAt: 1 } };
          const events = Array.from({ length: 520 }, (_, index) => ({ kind: 'text', text: 'synthetic row ' + (index + 1), seq: index + 1, at: 2000 }));
          const view = RelayOutput.mount({ node, api: async url => {
            const cursor = Number(new URL(url, 'http://127.0.0.1').searchParams.get('cursor') || 0);
            const page = events.filter(event => event.seq > cursor).slice(0, 200);
            const nextCursor = page.at(-1)?.seq || cursor;
            return { scope: { projectId: state.id, attemptId: state.activeAttempt.id, epoch: 1, runId: null, inputHash: 'c'.repeat(64) },
              events: page, nextCursor, hasMore: nextCursor < events.length, gap: false, sealed: false, available: true, serverNow: 3000,
              metadata: { role: 'architect', kind: 'generate', status: 'running', queuedAt: 1000, startedAt: 1500, pid: 123, stage: 'cli_running', lastEventAt: 2000 },
              authority: { phase: 'designing', epoch: 1, revision: 1, leaseUnknown: false, recoveryRequired: false, activeAttemptId: state.activeAttempt.id } };
          } });
          window.boundedOutput = { events, view, state };
          view.visible(true); view.setProject(state, true);
        });
        await isolated.waitForFunction(() => document.querySelector('#claude-output').lastChild?.dataset.seq === '520');
        assert.equal(await isolated.locator('#claude-output > div').count(), 512);
        assert.equal(await isolated.locator('#claude-output > div').first().getAttribute('data-seq'), '9');
        await isolated.locator('#claude-output').focus();
        await isolated.keyboard.press('Home');
        await isolated.waitForFunction(() => document.querySelector('#claude-follow').getAttribute('aria-pressed') === 'false'
          && document.querySelector('#claude-output').scrollTop <= 1);
        await isolated.evaluate(() => {
          const { events, view, state } = boundedOutput;
          events.push({ kind: 'text', text: 'manual reading marker', seq: 521, at: 2000 });
          view.setProject(state, false); view.setProject(state, true);
        });
        await isolated.waitForFunction(() => document.querySelector('#claude-output').lastChild?.dataset.seq === '521');
        assert.equal(await isolated.locator('#claude-follow').getAttribute('aria-pressed'), 'false');
        assert.ok(await isolated.locator('#claude-output').evaluate(el => el.scrollHeight - el.scrollTop - el.clientHeight > 24));
        const beforeFollow = await isolated.locator('#claude-output').evaluate(el => ({ top: el.scrollTop, height: el.scrollHeight, client: el.clientHeight, pressed: document.querySelector('#claude-follow').getAttribute('aria-pressed') }));
        await isolated.locator('#claude-follow').click();
        const afterFollow = await isolated.locator('#claude-output').evaluate(el => ({ top: el.scrollTop, height: el.scrollHeight, client: el.clientHeight, pressed: document.querySelector('#claude-follow').getAttribute('aria-pressed') }));
        assert.ok(afterFollow.height - afterFollow.top - afterFollow.client <= 1, JSON.stringify({ beforeFollow, afterFollow }));
        await isolated.evaluate(() => {
          const { events, view, state } = boundedOutput;
          for (let seq = 522; seq <= 721; seq++) events.push({ kind: 'text', text: '汉'.repeat(682), seq, at: 2000 });
          view.setProject(state, false); view.setProject(state, true);
        });
        await isolated.waitForFunction(() => document.querySelector('#claude-output').lastChild?.dataset.seq === '721');
        const retained = await isolated.locator('#claude-output').evaluate(el => ({
          nodes: el.children.length, bytes: [...el.children].reduce((sum, row) => sum + new TextEncoder().encode(row.textContent).length, 0)
        }));
        assert.ok(retained.nodes <= 512);
        assert.ok(retained.bytes <= 256 * 1024);
        assert.ok(retained.bytes > 250 * 1024, 'The retained tail must remain useful, not be cleared to satisfy the cap');
        await isolated.evaluate(() => boundedOutput.view.stop());
      } finally { await isolated.close(); }
    });
    await check('old-attempt-without-telemetry-stays-explicitly-unavailable', async () => {
      const isolated = await context.newPage();
      try {
        await isolated.setContent('<div class="log-tabs"></div><div id="event-log"></div>');
        await isolated.addScriptTag({ path: path.join(__dirname, '../../live-output.js') });
        await isolated.evaluate(() => {
          const node = (tag, classes = '', text = '') => Object.assign(document.createElement(tag), { className: classes, textContent: text });
          const view = RelayOutput.mount({ node, api: async () => ({ scope: null, events: [], nextCursor: 0, hasMore: false,
            gap: false, sealed: false, available: false, serverNow: 10000, metadata: {}, authority: { phase: 'designing' } }) });
          window.oldOutput = view;
          view.setProject({ id: 'a'.repeat(32), epoch: 1, activeAttempt: { id: 'b'.repeat(32), status: 'running', createdAt: 1 } }, true);
        });
        await isolated.waitForFunction(() => document.querySelector('#claude-status').textContent.includes('不能通过刷新转换'));
        assert.equal(await isolated.locator('#claude-output').textContent(), '');
        assert.match(await isolated.locator('#claude-timing').innerText(), /CLI 尚无可验证计时/);
        await isolated.evaluate(() => oldOutput.stop());
      } finally { await isolated.close(); }
    });
    await context.close();
  } catch (error) { console.error(String(error)); process.exitCode = 1; }
  finally { await browser.close(); fs.writeFileSync(path.join(output, 'browser-results.json'), JSON.stringify(results, null, 2)); }
  console.log(JSON.stringify(results));
})();
