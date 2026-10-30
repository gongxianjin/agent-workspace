const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { pathToFileURL } = require('node:url');
const [playwrightArg, chromiumArg] = process.argv.slice(2);
if (!playwrightArg || !chromiumArg || !path.isAbsolute(playwrightArg) || !path.isAbsolute(chromiumArg)) {
  throw Error('Use run_static_browser.py with trusted explicit browser dependencies');
}
const { chromium } = require(path.dirname(playwrightArg));
const root = path.resolve(__dirname, '..');
const output = process.env.RELAY_STATIC_OUTPUT ? path.resolve(process.env.RELAY_STATIC_OUTPUT) : path.join(root, 'dist/p0.2');
const url = pathToFileURL(path.join(root, 'index.html')).href;
const demo = pathToFileURL(path.join(root, 'demo/index.html')).href;
const results = [];
const upload = (page, text = '# 修改后的设计\n保留本地搜索与收藏。', name = 'reviewed.md') =>
  page.locator('#document-file').setInputFiles({ name, mimeType: 'text/plain', buffer: Buffer.from(text) });
const row = (page, id) => page.locator(`[data-task="${id}"]`);
const taskButton = (page, id, action) => row(page, id).locator(`[data-action="${action}"]`);
const phase = async (page, value) => assert.equal(await page.locator('body').getAttribute('data-phase'), value);
const disabled = async (page, id) => assert.ok(await page.locator('#' + id).isDisabled(), id + ' must be disabled');
async function settled(page) { await page.waitForFunction(() => document.body.dataset.uploading === 'false'); }
async function review(page) {
  await page.goto(url);
  await upload(page);
  await settled(page);
  await page.locator('#split-tasks').click();
}
async function approve(page) {
  await review(page);
  for (const id of ['D1', 'D2', 'T1']) await taskButton(page, id, 'approve').click();
  await phase(page, 'approved');
}
async function testing(page) {
  await approve(page);
  await page.locator('#start').click();
  await page.locator('#develop-complete').click();
}
async function controlReads(context) {
  await context.addInitScript(() => {
    const original = File.prototype.arrayBuffer;
    window.fileReads = new Map();
    File.prototype.arrayBuffer = function () {
      if (!this.name.startsWith('slow-')) return original.call(this);
      return new Promise((resolve, reject) => {
        window.fileReads.set(this.name, {
          resolve: () => original.call(this).then(resolve, reject),
          reject: () => reject(new Error('simulated file read error'))
        });
      });
    };
  });
}
const release = (page, name, mode = 'resolve') => page.evaluate(({ name, mode }) => window.fileReads.get(name)[mode](), { name, mode });

(async () => {
  fs.mkdirSync(output, { recursive: true });
  const browser = await chromium.launch({ headless: true,
    executablePath: chromiumArg });
  try {
    async function scenario(name, run, options = {}) {
      const context = await browser.newContext({ viewport: { width: 1440, height: 1080 }, acceptDownloads: true, ...options });
      context.setDefaultTimeout(5000);
      const errors = [], external = [];
      await context.route(/^https?:/, route => { external.push(route.request().url()); return route.abort(); });
      const page = await context.newPage();
      page.on('pageerror', error => errors.push(String(error)));
      try {
        await run(page, context);
        assert.deepEqual(errors, [], 'No uncaught app errors');
        assert.deepEqual(external, [], 'Static pages must not contact external services');
        results.push({ name, passed: true });
        console.log('PASS', name);
      } catch (error) {
        results.push({ name, passed: false, error: String(error) });
        console.error('FAIL', name, error);
      } finally { await context.close(); }
    }

    // Removing the document/file gates or restoring whole-list approval must break these scenarios.
    await scenario('file-open-and-document-before-tasks', async page => {
      await page.goto(url);
      await phase(page, 'draft');
      assert.equal(await page.locator('#workflow-map').count(), 1, 'A visual overview explains the complete handoff');
      assert.match(await page.locator('#workflow-map').innerText(), /全部任务通过/);
      assert.match(await page.locator('#workflow-map').innerText(), /只重做驳回项/);
      await disabled(page, 'start');
      await disabled(page, 'split-tasks');
      await disabled(page, 'download-document');
      assert.equal(await page.locator('#generate-document').count(), 1, 'The UI must generate a design before tasks');
      await page.locator('#generate-document').click();
      await phase(page, 'document');
      assert.match(await page.locator('#generated-preview').innerText(), /设计文档/);
      assert.equal(await page.locator('[data-task]').count(), 0);
      await disabled(page, 'split-tasks');
      for (const [role, client] of Object.entries({ architect: 'Claude Code', developer: 'Codex', tester: 'BoBot' })) {
        assert.equal(await page.locator('#role-' + role).inputValue(), client);
      }
      assert.match(await page.locator('#prototype-notice').innerText(), /未接入真实/);
      assert.equal(await page.locator('#approve').count(), 0, 'No whole-list approval shortcut');
    });

    await scenario('download-edit-upload-real-file-roundtrip', async page => {
      await page.goto(url);
      await page.locator('#generate-document').click();
      const expected = await page.locator('#generated-preview').textContent();
      const event = page.waitForEvent('download');
      await page.locator('#download-document').click();
      const download = await event;
      assert.match(download.suggestedFilename(), /\.md$/);
      const content = fs.readFileSync(await download.path(), 'utf8');
      assert.equal(content, expected, 'Downloaded bytes match the displayed generated design');
      await disabled(page, 'split-tasks');
      const edited = content + '\n## 人工补充\n先保证空列表和收藏回退。';
      await upload(page, edited, '人工审核设计.md');
      await settled(page);
      assert.equal(await page.locator('#upload-preview').textContent(), edited);
      assert.match(await page.locator('#uploaded-meta').innerText(), /U1.*人工审核设计\.md/);
      await page.locator('#split-tasks').click();
      assert.equal(await page.locator('[data-task]').count(), 3);
      assert.match(await page.locator('#task-simulation-note').innerText(), /未解析正文/);
      await disabled(page, 'start');
      await page.evaluate(() => window.scrollTo(0, 0));
      await page.screenshot({ path: path.join(output, 'workbench-desktop.png'), fullPage: true });
    });

    await scenario('per-task-approval-hard-gate', async page => {
      await review(page);
      for (const id of ['D1', 'D2']) {
        await taskButton(page, id, 'approve').click();
        await phase(page, 'review');
        await disabled(page, 'start');
      }
      await taskButton(page, 'T1', 'approve').click();
      await phase(page, 'approved');
      assert.ok(await page.locator('#start').isEnabled());
      assert.match(await page.locator('#task-count').innerText(), /3 \/ 3/);
      assert.match(await page.locator('#phase-title').innerText(), /等待/);
      await page.locator('#start').click();
      await phase(page, 'developing');
    });

    await scenario('reject-local-regenerate-preserve-and-dependent-rereview', async page => {
      await approve(page);
      const preserved = await row(page, 'D2').innerText();
      const version = await row(page, 'D1').getAttribute('data-version');
      assert.ok(await taskButton(page, 'D1', 'reject').isDisabled());
      await row(page, 'D1').locator('textarea').fill('补充空列表验收');
      await taskButton(page, 'D1', 'reject').click();
      await phase(page, 'review');
      await disabled(page, 'start');
      assert.match(await row(page, 'T1').innerText(), /需要重审/);
      await taskButton(page, 'D1', 'regenerate').click();
      assert.notEqual(await row(page, 'D1').getAttribute('data-version'), version);
      assert.match(await row(page, 'D1').innerText(), /待审核/);
      assert.match(await row(page, 'D1').locator('.task-detail').innerText(), /补充空列表验收/);
      assert.match(await row(page, 'D1').locator('.task-acceptance').innerText(), /补充空列表验收/);
      assert.equal(await row(page, 'D2').innerText(), preserved);
      await taskButton(page, 'D1', 'approve').click();
      await disabled(page, 'start');
      await page.screenshot({ path: path.join(output, 'workbench-rereview.png'), fullPage: true });
      await taskButton(page, 'T1', 'approve').click();
      await phase(page, 'approved');
    });

    await scenario('new-document-invalidates-all-old-approvals-and-same-file-can-repeat', async page => {
      await approve(page);
      await upload(page, '# 第二版设计', 'same.md');
      await settled(page);
      await phase(page, 'uploaded');
      assert.equal(await page.locator('[data-task]').count(), 0);
      await disabled(page, 'start');
      assert.match(await page.locator('#uploaded-meta').innerText(), /U2/);
      await upload(page, '# 第二版设计', 'same.md');
      await settled(page);
      assert.match(await page.locator('#uploaded-meta').innerText(), /U3/);
    });

    await scenario('invalid-files-preserve-approved-snapshot', async page => {
      await approve(page);
      const oldText = await page.locator('#upload-preview').textContent();
      const oldTasks = await page.locator('#tasks').innerText();
      for (const [name, buffer] of [
        ['bad.html', Buffer.from('#设计')], ['blank.md', Buffer.from(' \n\t')],
        ['binary.md', Buffer.from([0, 1, 2])], ['bad-utf8.txt', Buffer.from([0xc3, 0x28])],
        ['large.md', Buffer.alloc(256 * 1024 + 1, 97)]
      ]) {
        await page.locator('#document-file').setInputFiles({ name, mimeType: 'text/plain', buffer });
        await settled(page);
        assert.ok((await page.locator('#upload-error').textContent()).length > 0);
        assert.equal(await page.locator('#upload-preview').textContent(), oldText);
        assert.equal(await page.locator('#tasks').innerText(), oldTasks);
        assert.ok(await page.locator('#start').isEnabled());
      }
      await page.locator('#document-file').setInputFiles([]);
      assert.equal(await page.locator('#tasks').innerText(), oldTasks);
    });

    await scenario('whole-list-resplit-cancel-and-confirm', async page => {
      await approve(page);
      const oldTasks = await page.locator('#tasks').innerText();
      page.once('dialog', dialog => dialog.dismiss());
      await page.locator('#split-tasks').click();
      assert.equal(await page.locator('#tasks').innerText(), oldTasks);
      page.once('dialog', async dialog => {
        assert.match(dialog.message(), /批准.*清空/);
        await dialog.accept();
      });
      await page.locator('#split-tasks').click();
      await phase(page, 'review');
      await disabled(page, 'start');
      assert.equal(await page.locator('[data-status="pending"]').count(), 3);
    });

    await scenario('upload-pending-blocks-and-read-failure-restores', async (page, context) => {
      await controlReads(context);
      await approve(page);
      await upload(page, '#待上传', 'slow-fail.md');
      await page.waitForFunction(() => window.fileReads.has('slow-fail.md'));
      await disabled(page, 'start');
      await disabled(page, 'split-tasks');
      assert.ok(await taskButton(page, 'D1', 'reject').isDisabled());
      await release(page, 'slow-fail.md', 'reject');
      await settled(page);
      assert.match(await page.locator('#upload-error').innerText(), /读取失败/);
      await phase(page, 'approved');
      assert.ok(await page.locator('#start').isEnabled());
    });

    await scenario('upload-race-latest-selection-wins', async (page, context) => {
      await controlReads(context);
      await page.goto(url);
      await upload(page, '#旧文件', 'slow-first.md');
      await upload(page, '#最新文件', 'slow-latest.md');
      await release(page, 'slow-latest.md');
      await settled(page);
      await release(page, 'slow-first.md');
      assert.equal(await page.locator('#upload-preview').textContent(), '#最新文件');
      assert.match(await page.locator('#uploaded-meta').innerText(), /slow-latest/);
      assert.equal(await page.locator('#upload-error').textContent(), '');
    });

    await scenario('upload-reset-and-requirement-change-discard-late-read', async (page, context) => {
      await controlReads(context);
      await page.goto(url);
      await upload(page, '#旧项目', 'slow-reset.md');
      await page.locator('#reset').click();
      await release(page, 'slow-reset.md');
      await phase(page, 'draft');
      await disabled(page, 'split-tasks');
      assert.equal(await page.locator('#upload-preview').textContent(), '');
      await upload(page, '#换需求前', 'slow-edit.md');
      await page.locator('#requirement').fill('另一个项目');
      await release(page, 'slow-edit.md');
      await disabled(page, 'split-tasks');
      assert.equal(await page.locator('#upload-preview').textContent(), '');
    });

    await scenario('unsafe-file-and-rejection-text-are-not-executed', async page => {
      const payload = '<img src="https://example.invalid/inject" onerror="window.injected=1">';
      await page.goto(url);
      await page.locator('#requirement').fill(payload);
      await page.locator('#generate-document').click();
      assert.ok((await page.locator('#generated-preview').textContent()).includes(payload));
      await upload(page, '#设计\n' + payload, '安全<img>.md');
      await settled(page);
      await page.locator('#split-tasks').click();
      await row(page, 'D1').locator('textarea').fill(payload);
      await taskButton(page, 'D1', 'reject').click();
      await taskButton(page, 'D1', 'regenerate').click();
      assert.ok((await row(page, 'D1').innerText()).includes(payload));
      assert.equal(await page.locator('img').count(), 0);
      assert.equal(await page.evaluate(() => window.injected), undefined);
    });

    await scenario('requirement-or-client-edit-invalidates-running-result', async page => {
      await approve(page);
      await page.locator('#start').click();
      await disabled(page, 'document-file');
      await page.locator('#requirement').fill('改后的需求');
      await phase(page, 'draft');
      assert.equal(await page.locator('[data-task]').count(), 0);
      await disabled(page, 'develop-complete');
      await upload(page);
      await settled(page);
      await page.locator('#split-tasks').click();
      await page.locator('#role-developer').selectOption('BoBot');
      await phase(page, 'draft');
      await disabled(page, 'start');
      await disabled(page, 'split-tasks');
    });

    await scenario('failure-repair-retest-manual-acceptance', async page => {
      await testing(page);
      await page.locator('#test-fail').click();
      await phase(page, 'failed');
      await disabled(page, 'accept');
      await disabled(page, 'test-pass');
      await page.locator('#fix').click();
      await phase(page, 'fixing');
      await page.locator('#fix-complete').click();
      await page.locator('#test-pass').click();
      await phase(page, 'ready');
      await page.locator('#accept').click();
      await phase(page, 'accepted');
      assert.match(await page.locator('#delivery-status').innerText(), /模拟验收/);
      await page.screenshot({ path: path.join(output, 'workbench-delivery.png'), fullPage: true });
    });

    await scenario('pause-stop-reset-and-duplicate-start', async page => {
      await approve(page);
      await page.locator('#start').evaluate(button => { button.click(); button.click(); });
      await phase(page, 'developing');
      await page.locator('#pause').click();
      await disabled(page, 'develop-complete');
      await page.locator('#resume').click();
      await phase(page, 'developing');
      await page.locator('#stop').click();
      await phase(page, 'stopped');
      await disabled(page, 'start');
      await page.locator('#reset').click();
      await phase(page, 'draft');
      assert.equal(await page.locator('[data-task]').count(), 0);
    });

    await scenario('mobile-keyboard-review-long-text-and-tabs', async page => {
      await review(page);
      assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth));
      await taskButton(page, 'D1', 'approve').focus();
      await page.keyboard.press('Enter');
      assert.equal(await row(page, 'D1').getAttribute('data-status'), 'approved');
      await row(page, 'D2').locator('textarea').fill('长'.repeat(400));
      assert.equal(await row(page, 'D2').locator('textarea').inputValue(), '长'.repeat(400));
      await taskButton(page, 'D2', 'reject').click();
      await taskButton(page, 'D2', 'regenerate').click();
      await page.locator('#tab-workflow').focus();
      await page.keyboard.press('ArrowRight');
      assert.equal(await page.locator('#tab-reason').getAttribute('aria-selected'), 'true');
      assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth));
      await page.screenshot({ path: path.join(output, 'workbench-mobile.png'), fullPage: true });
    }, { viewport: { width: 375, height: 812 } });

    await scenario('sample-search-filter-favorites-persist', async page => {
      await page.goto(demo);
      assert.equal(await page.locator('.place-card').count(), 6);
      assert.match(await page.locator('#sample-notice').innerText(), /非本轮/);
      await page.locator('#search').fill('徐汇');
      assert.equal(await page.locator('.place-card').count(), 1);
      await page.locator('#search').fill('');
      await page.locator('[data-category="culture"]').click();
      assert.equal(await page.locator('.place-card').count(), 2);
      await page.locator('.favorite-button').first().click();
      await page.locator('#favorites-only').check();
      assert.equal(await page.locator('.place-card').count(), 1);
      await page.reload();
      await page.locator('#favorites-only').check();
      assert.equal(await page.locator('.place-card').count(), 1);
      await page.locator('#search').fill('不存在的地点');
      assert.ok(await page.locator('#empty').isVisible());
    });

    await scenario('sample-storage-disabled-and-mobile', async (page, context) => {
      await context.addInitScript(() => {
        Object.defineProperty(window, 'localStorage', { get() { throw new Error('storage disabled'); } });
      });
      await page.goto(demo);
      await page.locator('.favorite-button').first().click();
      await page.locator('#favorites-only').check();
      assert.equal(await page.locator('.place-card').count(), 1);
      assert.match(await page.locator('#storage-notice').innerText(), /内存/);
      assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth));
    }, { viewport: { width: 375, height: 812 } });
  } finally {
    await browser.close();
    fs.writeFileSync(path.join(output, 'browser-results.json'), JSON.stringify({
      scope: 'P0.2 local static UI tests; not BoBot, CLI, AI or reason-network integration', results
    }, null, 2));
  }
  assert.ok(results.length >= 17 && results.every(result => result.passed), 'Every browser scenario must pass');
})().catch(error => { console.error(error); process.exitCode = 1; });
