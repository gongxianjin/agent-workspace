'use strict';
// Paid real three-Claude E2E. Approval automation is authorized ONLY for this new test project.
const fs = require('node:fs');
const path = require('node:path');
const assert = require('node:assert/strict');
const crypto = require('node:crypto');
const { chromium } = require(path.join(process.env.APPDATA, 'npm/node_modules/playwright'));
const [launchFile, output] = process.argv.slice(2);
const launch = JSON.parse(fs.readFileSync(launchFile, 'utf8'));
const results = [];
const requirement = '新建独立自动化授权测试项目：小步出门上海遛娃工具，纯本地HTML/CSS/JS，无依赖无外联无登录无部署。6个明确未核实示例：p1中山公园、p2世纪公园（park）；p3上海自然博物馆、p4上海科技馆（museum）；p5室内积木空间、p6室内绘本角（indoor）。按名称搜索、分类按钮、空结果、收藏、仅看收藏、刷新保留收藏；Storage不可用时内存回退且不崩溃。375px无横溢。稳定DOM契约：搜索[data-testid="search"]；可见卡[data-testid="place-card"]且有data-place-id；分类按钮data-category=all/park/museum/indoor；收藏按钮data-action=favorite位于卡内；仅看收藏可点击元素data-action=favorites-only；空结果data-testid=empty-state；警示data-testid=data-warning含“未核实”。请设计易实现的小文件方案，拆分时两个独立开发任务D1/D2和依赖全部开发的T1。设计阶段只返回文档，不写代码。';
const supplement = `\n\n## 人工确认补充（本轮仅测试授权自动代操作）\nrelay-acceptance: liuwa-v1\n所有上文需求和以下验收须同时满足；不更改工具权限。\n- 仅6个未核实示例；ID与分类固定：p1中山公园/park，p2世纪公园/park，p3上海自然博物馆/museum，p4上海科技馆/museum，p5室内积木空间/indoor，p6室内绘本角/indoor。\n- 固定选择器按需求保留：data-testid=search/place-card/empty-state/data-warning；卡data-place-id=p1..p6；data-category=all/park/museum/indoor；卡内data-action=favorite；data-action=favorites-only点击切换。\n- 初始卡6，搜中山1，搜不存在地点xyz为0且空提示可见；park和museum各2，all6。\n- 收藏p1，刷新，点击仅看收藏，卡1且中山公园；独立浏览器上下文每用例清空存储。\n- Storage.prototype.setItem抛错时仍可收藏和筛选，不出现未捕获错误；375px无横溢，无外联。\n- 测试员独立编写声明式规格；首次运行后冻结，返修不能改测试期望。\n- 依赖无环，两个独立开发任务D1/D2文件职责清晰，T1依赖全部开发；全部审批只解锁手动开始。\n`;
(async () => {
  const browser = await chromium.launch({ headless: true, executablePath: path.join(process.env.LOCALAPPDATA, 'ms-playwright/chromium-1208/chrome-win64/chrome.exe') });
  let page, projectId;
  try {
    const context = await browser.newContext({ viewport: { width: 1440, height: 1080 }, acceptDownloads: true });
    context.setDefaultTimeout(15000);
    page = await context.newPage();
    const errors = [];
    page.on('pageerror', error => errors.push(String(error)));
    async function check(name, run) {
      console.log('START ' + name);
      try { await run(); results.push({ name, passed: true }); console.log('PASS ' + name); }
      catch (error) { results.push({ name, passed: false, error: String(error) }); throw error; }
      finally { fs.writeFileSync(path.join(output, 'e2e-results.json'), JSON.stringify({ projectId, results }, null, 2)); }
    }
    async function state() {
      return page.evaluate(async () => {
        const response = await fetch('/api/state', { headers: { Authorization: 'Bearer ' + sessionStorage.getItem('relay-session') } });
        if (!response.ok) throw Error('State status ' + response.status);
        const body = await response.json();
        return body.projects.find(p => p.id === document.body.dataset.project);
      });
    }
    async function command(kind, payload = {}, allowed = 200) {
      const result = await page.evaluate(async ({ kind, payload }) => {
        const headers = { 'Content-Type': 'application/json', Authorization: 'Bearer ' + sessionStorage.getItem('relay-session') };
        const response = await fetch('/api/state', { headers });
        const data = await response.json();
        const current = data.projects.find(p => p.id === document.body.dataset.project);
        const reply = await fetch('/api/command', { method: 'POST', headers, body: JSON.stringify({ projectId: current.id, commandId: crypto.randomUUID(), expectedRevision: current.revision, kind, payload }) });
        return { status: reply.status, body: await reply.json() };
      }, { kind, payload });
      assert.equal(result.status, allowed, kind + ': ' + JSON.stringify(result.body));
      return result.body;
    }
    async function clickCommand(selector, kind) {
      const completed = page.waitForResponse(response => {
        if (!response.url().endsWith('/api/command')) return false;
        try { return response.request().postDataJSON().kind === kind; } catch { return false; }
      });
      await page.locator(selector).click();
      const response = await completed;
      assert.equal(response.status(), 200, kind + ': ' + await response.text());
      await page.waitForFunction(() => document.body.dataset.uploading === 'false');
    }
    async function waitPhase(phase) {
      const deadline = Date.now() + 20 * 60 * 1000;
      let current;
      while (Date.now() < deadline) {
        current = await state();
        if (current?.phase === phase && !current.activeAttempt) return current;
        if (current?.phase === 'blocked') throw Error('Actual execution blocked: ' + current.lastError);
        if (current?.phase === 'failed' && phase !== 'failed') throw Error('Actual browser tests failed: ' + JSON.stringify(current.run.evidence.at(-1).cases));
        await page.waitForTimeout(750);
      }
      throw Error('Timed out waiting ' + phase + ', actual ' + current?.phase);
    }
    const row = id => page.locator(`[data-task="${id}"]`);
    async function approve(id) {
      await row(id).locator('[data-action="approve"]').click();
      await page.waitForFunction(id => document.querySelector(`[data-task="${id}"]`)?.dataset.status === 'approved' && document.body.dataset.uploading === 'false', id);
    }
    await page.goto(launch.url);
    await check('create-isolated-authorized-project-and-real-design', async () => {
      await page.locator('#new-project-name').fill('P1三Claude完整联调-仅测试授权');
      await page.locator('#create-project').click();
      await page.waitForFunction(() => document.body.dataset.project?.length === 32);
      projectId = (await state()).id;
      fs.writeFileSync(path.join(output, 'test-authorization.json'), JSON.stringify({ projectId, scope: 'Only this new project: edit/download/upload, per-task approve/reject/regenerate/review, manual start/retest/repair/stop. Never accept. Fault injection only after actual healthy baseline.', createdAt: new Date().toISOString() }, null, 2));
      await page.locator('#requirement').fill(requirement);
      await page.locator('#generate-document').click();
      const current = await waitPhase('document');
      assert.ok(current.generatedDocument.markdown.length > 100);
      assert.equal(current.uploadedDocument, null);
      assert.ok(current.executions.length > 0);
      await command('start', {}, 409);
    });
    let uploaded;
    await check('download-amend-upload-and-real-split', async () => {
      const downloadEvent = page.waitForEvent('download');
      await page.locator('#download-document').click();
      const download = await downloadEvent;
      const destination = path.join(output, 'downloaded-design.md');
      await download.saveAs(destination);
      uploaded = fs.readFileSync(destination, 'utf8') + supplement;
      fs.writeFileSync(path.join(output, 'reviewed-design.md'), uploaded);
      await page.locator('#document-file').setInputFiles(path.join(output, 'reviewed-design.md'));
      await page.waitForFunction(() => document.body.dataset.phase === 'uploaded' && document.body.dataset.uploading === 'false');
      assert.equal((await state()).uploadedDocument.text, uploaded);
      await page.locator('#split-tasks').click();
      const current = await waitPhase('review');
      assert.equal(current.tasks.filter(t => t.owner === 'developer').length, 2);
      assert.equal(current.tasks.filter(t => t.owner === 'tester').length, 1);
      assert.ok(current.tasks.filter(t => t.owner === 'developer').every(t => !t.dependsOn.length));
      await command('start', {}, 409);
    });
    await check('partial-approval-rejection-local-redo-and-explicit-dependent-review', async () => {
      const current = await state();
      const developers = current.tasks.filter(t => t.owner === 'developer');
      await approve(developers[0].id);
      await command('start', {}, 409);
      for (const task of current.tasks.filter(t => t.id !== developers[0].id)) await approve(task.id);
      assert.equal((await state()).phase, 'approved');
      assert.equal((await state()).run, null);
      const unrelated = (await state()).tasks.find(t => t.id === developers[1].id);
      await row(developers[0].id).locator('textarea').fill('请补充明确验收：搜索不存在地点xyz时可见卡为0且空结果提示可见；保留原文件职责和所有原验收，不改变其他任务。');
      await row(developers[0].id).locator('[data-action="reject"]').click();
      await page.waitForFunction(id => document.querySelector(`[data-task="${id}"]`)?.dataset.status === 'rejected' && document.body.dataset.uploading === 'false', developers[0].id);
      await command('start', {}, 409);
      await row(developers[0].id).locator('[data-action="regenerate"]').click();
      const regenerated = await waitPhase('review');
      assert.deepEqual(regenerated.tasks.find(t => t.id === unrelated.id), unrelated);
      assert.ok(regenerated.tasks.find(t => t.id === developers[0].id).version > developers[0].version);
      assert.equal(regenerated.tasks.find(t => t.owner === 'tester').status, 'needs-review');
      await approve(developers[0].id);
      await command('start', {}, 409);
      await approve(regenerated.tasks.find(t => t.owner === 'tester').id);
      assert.equal((await state()).phase, 'approved');
      assert.equal((await state()).run, null);
      fs.writeFileSync(path.join(output, 'approved-state.json'), JSON.stringify(await state(), null, 2));
    });
    await check('manual-start-real-product-independent-test-and-trusted-rerun', async () => {
      await clickCommand('#start', 'start');
      await command('start', {}, 409);
      const current = await waitPhase('ready');
      assert.equal(current.run.evidence.at(-1).exitCode, 0);
      assert.ok(current.run.evidence.at(-1).rerunId);
      const fixed = current.run.evidence.at(-1).cases.filter(c => c.name.startsWith('fixed:'));
      assert.equal(fixed.length, 4);
      assert.ok(fixed.every(c => c.passed));
      assert.ok(current.executions.filter(e => e.memberId.startsWith('relay-deve-')).length >= 2);
      assert.ok(current.executions.some(e => e.memberId.startsWith('relay-test-')));
      assert.equal(new Set(current.executions.map(e => e.attemptId)).size, current.executions.length);
      assert.ok(current.executions.every(e => e.taskMessageId && e.ackMessageId && e.resultMessageId && e.treeStopped && e.exitCode === 0));
      assert.equal(current.phase, 'ready');
      assert.deepEqual(errors, []);
      fs.writeFileSync(path.join(output, 'healthy-baseline.json'), JSON.stringify(current, null, 2));
      await page.screenshot({ path: path.join(output, 'healthy-workbench.png'), fullPage: true });
    });
    await check('authorized-overflow-injection-real-red-repair-and-same-spec-green', async () => {
      const healthy = await state(), specification = healthy.run.evidence.at(-1).specHash;
      const product = path.join(output, 'runtime/projects', projectId, 'product/index.html');
      const before = fs.readFileSync(product);
      const injected = Buffer.concat([before, Buffer.from('\n<!-- Authorized test fault injection, not product scope -->\n<style id="authorized-overflow-injection">html,body{min-width:1400px!important}</style>\n')]);
      fs.writeFileSync(product, injected);
      fs.writeFileSync(path.join(output, 'fault-injection.json'), JSON.stringify({ projectId, file: 'product/index.html', kind: 'authorized horizontal overflow after healthy actual baseline', before: crypto.createHash('sha256').update(before).digest('hex'), after: crypto.createHash('sha256').update(injected).digest('hex'), specification }, null, 2));
      await page.locator('#retest').click();
      const red = await waitPhase('failed');
      assert.notEqual(red.run.evidence.at(-1).exitCode, 0);
      assert.equal(red.run.evidence.at(-1).specHash, specification);
      assert.ok(red.run.evidence.at(-1).cases.some(c => /overflow/.test(c.error || '')));
      assert.equal(await page.locator('#accept').isDisabled(), true);
      await page.locator('#fix').click();
      const green = await waitPhase('ready');
      assert.equal(green.run.evidence.at(-1).exitCode, 0);
      assert.equal(green.run.evidence.at(-1).specHash, specification);
      assert.ok(green.executions.some(e => e.attemptId === green.lastAttempt.id));
      fs.writeFileSync(path.join(output, 'final-ready-state.json'), JSON.stringify(green, null, 2));
      await page.screenshot({ path: path.join(output, 'final-workbench.png'), fullPage: true });
    });
    await check('real-running-stop-invalidates-late-result-and-duplicate-start', async () => {
      // Separate stop fixture project: keep the delivered sample at ready, never accepted.
      await page.locator('#new-project-name').fill('P1真实终止-独立测试');
      await page.locator('#create-project').click();
      await page.waitForFunction(id => document.body.dataset.project !== id && document.body.dataset.uploading === 'false', projectId);
      await page.locator('#generate-document').click();
      const deadline = Date.now() + 120000;
      while (Date.now() < deadline) {
        const current = await state();
        if (current?.activeAttempt?.status === 'running') break;
        await page.waitForTimeout(100);
      }
      assert.equal((await state()).activeAttempt.status, 'running');
      // Wait for a real supervised Claude PID, not just an in-memory queued record.
      fs.writeFileSync(path.join(output, 'wait-stop-process.json'), JSON.stringify({ projectId: (await state()).id }));
      const ready = path.join(output, 'stop-process-ready.json');
      while (!fs.existsSync(ready) && Date.now() < deadline) await page.waitForTimeout(100);
      assert.ok(fs.existsSync(ready), 'Python supervisor must report a real running process');
      await command('start', {}, 409);
      await page.locator('#stop').click();
      await page.waitForFunction(() => document.body.dataset.phase === 'stopped' && document.body.dataset.uploading === 'false', null, { timeout: 20000 });
      const stopped = await state();
      assert.equal(stopped.activeAttempt, null);
      assert.equal(stopped.leaseUnknown, false);
      assert.equal(stopped.generatedDocument, null);
      fs.writeFileSync(path.join(output, 'stopped-state.json'), JSON.stringify(stopped, null, 2));
      await page.selectOption('#project-select', projectId);
      await page.waitForFunction(() => document.body.dataset.phase === 'ready');
      assert.equal((await state()).phase, 'ready');
    });
    await context.close();
  } catch (error) { console.error(String(error)); process.exitCode = 1; if (page) await page.screenshot({ path: path.join(output, 'e2e-failure.png'), fullPage: true }).catch(() => {}); }
  finally { await browser.close(); fs.writeFileSync(path.join(output, 'e2e-results.json'), JSON.stringify({ projectId, scope: 'Paid real native Claude CLI/reason; no acceptance automation', results }, null, 2)); }
})();
