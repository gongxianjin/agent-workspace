'use strict';
const fs = require('node:fs');
const path = require('node:path');
const assert = require('node:assert/strict');
const [launchFile, output, playwright, executablePath, source] = process.argv.slice(2);
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
    await check('real authenticated UI saves literal guidance, unknown POST reconciles read-only across role switch', async () => {
      const context = await browser.newContext({ viewport: { width: 375, height: 812 } });
      const page = await context.newPage(), errors = []; let posts = 0, drop = false;
      page.on('pageerror', error => errors.push(String(error)));
      await context.route('**/api/command', async route => {
        posts++; const response = await route.fetch();
        if (drop) { drop = false; await route.abort('failed'); } else await route.fulfill({ response });
      });
      try {
        await page.goto(urls[0]);
        await page.waitForFunction(() => document.body.dataset.connected === 'true');
        await page.locator('#agent-management').waitFor({ timeout: 2500 });
        assert.equal(await page.locator('[data-agent-role]').count(), 3);
        await page.locator('[data-agent-role="developer"]').click();
        const literal = '<img src=x onerror="window.agentInjected=1"> 只保存消息，不启动模型';
        await page.locator('#agent-draft').fill(literal);
        await page.locator('#agent-send').click();
        await page.waitForFunction(text => document.querySelector('#agent-thread')?.textContent.includes(text), literal);
        assert.match(await page.locator('#agent-thread').innerText(), /已保存，等待收件/);
        assert.equal(await page.locator('#agent-thread img').count(), 0);
        assert.equal(await page.evaluate(() => window.agentInjected), undefined);
        assert.equal(posts, 1);
        drop = true;
        await page.locator('#agent-draft').fill('仅一次提交的未知结果'); await page.locator('#agent-send').click();
        await page.waitForFunction(() => document.querySelector('#agent-command-status')?.textContent.includes('待核实'));
        await page.locator('[data-agent-role="architect"]').click();
        assert.equal(await page.locator('#agent-send').isDisabled(), true);
        assert.equal(await page.locator('#agent-consult').isDisabled(), true);
        assert.equal(await page.locator('#generate-document').isDisabled(), true, 'Unknown Agent submission must lock related workflow controls');
        await page.evaluate(() => document.getElementById('generate-document').dispatchEvent(new MouseEvent('click')));
        assert.equal(posts, 2, 'Even an already-queued workflow click must not bypass the project pending lock');
        await page.locator('#agent-reconcile').click();
        await page.waitForFunction(() => !document.querySelector('#agent-command-status')?.textContent.includes('待核实'));
        await page.locator('[data-agent-role="developer"]').click();
        await page.waitForFunction(() => document.querySelector('#agent-thread')?.textContent.includes('仅一次提交的未知结果'));
        assert.equal(posts, 2);
        assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true);
        await page.evaluate(() => scrollTo(0, 0));
        await page.screenshot({ path: path.join(output, 'agent-controls-mobile.png'), fullPage: true });
        await page.setViewportSize({ width: 1440, height: 1000 });
        assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true);
        await page.evaluate(() => scrollTo(0, 0));
        await page.screenshot({ path: path.join(output, 'agent-controls-desktop.png'), fullPage: true });
        assert.deepEqual(errors, []);
      } finally { await context.unrouteAll({ behavior: 'wait' }); await context.close(); }
    });
    await check('isolated real DOM controls disclose handoff, consultation and fresh explicit resume without auto POST', async () => {
      const page = await browser.newPage();
      try {
        await page.route('**/agent-controls-fixture', route => route.fulfill({ status: 200, contentType: 'text/html', body: '<section id="current-progress"></section>' }));
        await page.goto(new URL('/agent-controls-fixture', urls[0]).href);
        await page.clock.install(); await page.clock.pauseAt(new Date());
        await page.addScriptTag({ path: path.join(source, 'live-session.js') });
        assert.ok(fs.existsSync(path.join(source, 'live-agents.js')), 'Agent control module is missing');
        await page.addScriptTag({ path: path.join(source, 'live-agents.js') });
        assert.equal(await page.evaluate(() => typeof RelayAgents.mount), 'function');
        await page.evaluate(() => {
          const node = (tag, classes = '', text = '') => Object.assign(document.createElement(tag), { className: classes, textContent: text });
          const project = { id: 'a'.repeat(32), revision: 3, epoch: 1, phase: 'blocked', tasks: [], activeAttempt: null,
            agentControl: { role: 'developer', status: 'blocked', resumable: true, reason: '缺少接口约定' } };
          const data = { project, posts: [], confirmations: [], messages: { architect: [], developer: [{ id: 'question', seq: 1, role: 'developer', status: 'received', intent: 'clarification', author: 'user', text: '保留当前接口', summary: '已收件', targetAttemptId: 'old' }], tester: [] } };
          window.confirm = text => { data.confirmations.push(text); return true; };
          data.view = RelayAgents.mount({ node, storage: RelaySession.safeStorage(() => null), onState: state => { data.project = state; data.view.setProject(state, true, true); }, api: async (route, body) => {
            if (!body) { const role = new URL(route, 'http://fixture.invalid').searchParams.get('role'); return { projectId: project.id, role, messages: data.messages[role], hasMore: false, nextCursor: 1 }; }
            data.posts.push(body);
            return { ...data.project, revision: data.project.revision + 1 };
          } });
          window.agentFixture = data; data.view.setProject(project, true, true);
        });
        await page.locator('[data-agent-role="developer"]').click();
        await page.waitForFunction(() => document.querySelector('#agent-thread').textContent.includes('已收件'));
        assert.match(await page.locator('#agent-control-reason').innerText(), /缺少接口约定/);
        assert.equal(await page.evaluate(() => agentFixture.posts.length), 0);
        await page.locator('#agent-resume').click();
        assert.equal(await page.evaluate(() => agentFixture.posts.at(-1).kind), 'agent_resume');
        assert.deepEqual(await page.evaluate(() => agentFixture.posts.at(-1).payload.messageIds), []);
        assert.match(await page.evaluate(() => agentFixture.confirmations.at(-1)), /新.*可能计费/);
        await page.locator('[data-agent-role="architect"]').click();
        assert.equal(await page.locator('#agent-consult').isDisabled(), true, 'A question must be explicitly selected');
        await page.evaluate(() => {
          agentFixture.messages.architect = [{ id: 'q2', seq: 2, role: 'architect', status: 'queued', intent: 'clarification', author: 'user', text: '请确认接口设计', summary: '' }];
        });
        await page.clock.runFor(1300);
        await page.waitForFunction(() => !!document.querySelector('[data-message-id="q2"]'));
        await page.locator('[data-message-id="q2"] input[type=checkbox]').check();
        await page.locator('#agent-consult').click();
        assert.equal(await page.evaluate(() => agentFixture.posts.at(-1).kind), 'agent_consult');
        assert.deepEqual(await page.evaluate(() => agentFixture.posts.at(-1).payload), { role: 'architect', messageIds: ['q2'] });
        assert.match(await page.evaluate(() => agentFixture.confirmations.at(-1)), /只读.*可能计费.*不会自动恢复/);
        await page.evaluate(() => agentFixture.view.setProject({ ...agentFixture.project, activeAttempt: { id: 'b'.repeat(32), role: 'developer', status: 'running', kind: 'develop', taskId: 'D1' }, phase: 'developing' }, true, true));
        await page.locator('#agent-handoff').click();
        assert.equal(await page.evaluate(() => agentFixture.posts.at(-1).kind), 'agent_handoff');
        assert.match(await page.locator('#agent-command-status').innerText(), /请求.*不代表.*停稳/);
        await page.evaluate(() => {
          agentFixture.project = { ...agentFixture.project, activeAttempt: null, phase: 'blocked', agentInbox: { architect: { needs_confirmation: 1 }, developer: { needs_confirmation: 0 } } };
          agentFixture.view.setProject(agentFixture.project, true, true);
        });
        await page.locator('[data-agent-role="developer"]').click();
        assert.equal(await page.locator('#agent-resume').isDisabled(), true, 'Other-role scope conflicts must block a fresh paid developer resume');
        assert.match(await page.locator('#agent-control-hint').innerText(), /架构师.*范围.*恢复/);
        const beforeOtherConflict = await page.evaluate(() => agentFixture.posts.length);
        await page.evaluate(() => {
          agentFixture.project = { ...agentFixture.project, agentInbox: { architect: { needs_confirmation: 0 }, developer: { needs_confirmation: 0 } } };
          agentFixture.view.setProject(agentFixture.project, true, true);
        });
        assert.equal(await page.locator('#agent-resume').isDisabled(), false, 'Explicit resolution restores the original-role resume option');
        assert.equal(await page.evaluate(() => agentFixture.posts.length), beforeOtherConflict, 'Resolving display state cannot automatically resume');
        await page.evaluate(() => {
          const value = agentFixture;
          value.project = { ...value.project, activeAttempt: null, phase: 'blocked', agentInbox: { developer: { needs_confirmation: 1 } } };
          value.messages.developer = [{ id: 'question', seq: 1, role: 'developer', status: 'applied', intent: 'clarification', author: 'user', text: '保留当前接口', summary: '范围内已应用' },
            { id: 'conflict', seq: 3, role: 'developer', status: 'needs_confirmation', intent: 'scope_change', author: 'user', text: '推翻已批准设计', summary: '' }];
          value.view.setProject(value.project, true, true);
        });
        await page.locator('[data-agent-role="developer"]').click();
        await page.clock.runFor(1300);
        await page.waitForFunction(() => document.querySelector('#agent-thread').textContent.includes('范围内已应用'));
        assert.match(await page.locator('[data-message-id="conflict"]').innerText(), /聊天不能批准范围变更/);
        assert.equal(await page.locator('#agent-resume').isDisabled(), true);
        const beforeConflict = await page.evaluate(() => agentFixture.posts.length);
        await page.locator('[data-message-id="conflict"] button').first().click();
        assert.equal(await page.evaluate(() => agentFixture.posts.length), beforeConflict, 'No explanation draft means no resolution POST');
        await page.locator('#agent-draft').fill('只解释当前接口，不改变批准范围');
        await page.locator('[data-message-id="conflict"] button').first().click();
        assert.deepEqual(await page.evaluate(() => agentFixture.posts.at(-1).payload), { messageId: 'conflict', decision: 'clarification', text: '只解释当前接口，不改变批准范围' });
        await page.evaluate(() => {
          agentFixture.messages.architect = [{ id: 'reply', seq: 4, role: 'architect', status: 'answered', intent: 'reply', author: 'agent', text: '咨询建议草稿', summary: '' }];
        });
        await page.locator('[data-agent-role="architect"]').click();
        await page.waitForFunction(() => !!document.querySelector('[data-message-id="reply"]'));
        const beforeQuote = await page.evaluate(() => agentFixture.posts.length);
        await page.locator('[data-message-id="reply"] button').click();
        assert.equal(await page.locator('#agent-draft').inputValue(), '咨询建议草稿');
        assert.equal(await page.evaluate(() => agentFixture.posts.length), beforeQuote, 'Quoting does not send or apply advice automatically');
        assert.equal(await page.locator('[data-agent-role="developer"]').getAttribute('aria-pressed'), 'true');
        await page.evaluate(() => agentFixture.view.stop());
      } finally { await page.close(); }
    });
    await check('role/project/attempt late threads are discarded, drafts survive and 403 permanently stops reads', async () => {
      const page = await browser.newPage();
      try {
        await page.route('**/agent-controls-fixture', route => route.fulfill({ status: 200, contentType: 'text/html', body: '<section id="current-progress"></section>' }));
        await page.goto(new URL('/agent-controls-fixture', urls[0]).href);
        await page.clock.install(); await page.clock.pauseAt(new Date());
        await page.addScriptTag({ path: path.join(source, 'live-session.js') });
        await page.addScriptTag({ path: path.join(source, 'live-agents.js') });
        await page.evaluate(() => {
          const node = (tag, classes = '', text = '') => Object.assign(document.createElement(tag), { className: classes, textContent: text });
          const value = { pending: [], calls: 0, posts: 0, state: { id: 'a'.repeat(32), epoch: 1, revision: 1, tasks: [], activeAttempt: null } };
          value.view = RelayAgents.mount({ node, storage: RelaySession.safeStorage(() => null), onState() {}, api: (route, body) => {
            if (body) { value.posts++; return Promise.reject(Error('unexpected POST')); }
            value.calls++; const url = new URL(route, location.href);
            return new Promise((resolve, reject) => value.pending.push({ resolve, reject, role: url.searchParams.get('role'), id: url.searchParams.get('project') }));
          } });
          value.reply = (request, text) => request.resolve({ projectId: request.id, role: request.role, messages: [{ id: 'fixture', seq: 1, text, status: 'queued', intent: 'clarification', author: 'user' }], nextCursor: 1, hasMore: false });
          window.lateAgent = value; value.view.setProject(value.state, true, true);
        });
        await page.locator('#agent-draft').fill('架构师内存草稿');
        await page.locator('[data-agent-role="developer"]').click();
        await page.evaluate(() => lateAgent.reply(lateAgent.pending.shift(), '过期架构师响应'));
        await page.clock.runFor(1);
        assert.doesNotMatch(await page.locator('#agent-thread').innerText(), /过期架构师/);
        await page.evaluate(() => lateAgent.reply(lateAgent.pending.shift(), '当前开发者指导'));
        await page.clock.runFor(1);
        assert.match(await page.locator('#agent-thread').innerText(), /当前开发者指导/);
        await page.locator('#agent-draft').fill('开发者草稿');
        await page.locator('[data-agent-role="architect"]').click();
        assert.equal(await page.locator('#agent-draft').inputValue(), '架构师内存草稿');
        await page.evaluate(() => {
          lateAgent.state = { ...lateAgent.state, id: 'b'.repeat(32) };
          lateAgent.view.setProject(lateAgent.state, true, true);
          lateAgent.reply(lateAgent.pending.shift(), '上个项目迟到响应');
        });
        await page.clock.runFor(1);
        assert.doesNotMatch(await page.locator('#agent-thread').innerText(), /上个项目/);
        assert.equal(await page.locator('#agent-draft').inputValue(), '');
        await page.evaluate(() => {
          lateAgent.state = { ...lateAgent.state, activeAttempt: { id: 'c'.repeat(32), role: 'developer', kind: 'develop', status: 'queued' } };
          lateAgent.view.setProject(lateAgent.state, true, true);
          lateAgent.reply(lateAgent.pending.shift(), '旧阶段迟到响应');
        });
        await page.clock.runFor(1);
        assert.doesNotMatch(await page.locator('#agent-thread').innerText(), /旧阶段/);
        await page.evaluate(() => lateAgent.pending.shift().reject(Object.assign(Error('auth expired fixture'), { category: 'http', status: 403 })));
        await page.clock.runFor(1);
        const calls = await page.evaluate(() => lateAgent.calls);
        await page.evaluate(() => lateAgent.view.setProject(lateAgent.state, true, true));
        await page.clock.runFor(30000);
        assert.equal(await page.evaluate(() => lateAgent.calls), calls);
        assert.equal(await page.evaluate(() => lateAgent.posts), 0);
        assert.equal(await page.locator('#agent-send').isDisabled(), true);
        assert.match(await page.locator('#agent-thread-status').innerText(), /停止读取/);
      } finally { await page.close(); }
    });
  } finally { await browser.close(); }
  fs.writeFileSync(path.join(output, 'agent-controls-results.json'), JSON.stringify(results, null, 2));
  console.log(JSON.stringify(results)); process.exitCode = results.every(row => row.passed) ? 0 : 1;
})().catch(error => { console.error(error); process.exitCode = 1; });
