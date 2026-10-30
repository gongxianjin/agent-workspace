(function () {
  'use strict';
  // Inert in file:// mode. The live controller never calls model.transition().
  window.RelayLive = { mount };
  // commandId 幂等标识：安全上下文（HTTPS/localhost）用 randomUUID，普通 HTTP 部署回退到 CSP 安全的随机实现。
  function makeCommandId() {
    if (typeof crypto !== 'undefined' && typeof crypto.randomUUID === 'function') return crypto.randomUUID();
    if (typeof crypto !== 'undefined' && crypto.getRandomValues) {
      const bytes = crypto.getRandomValues(new Uint8Array(16));
      bytes[6] = bytes[6] & 0x0f | 0x40; bytes[8] = bytes[8] & 0x3f | 0x80; // UUID v4 形态
      const hex = [...bytes].map(b => b.toString(16).padStart(2, '0')).join('');
      return `${hex.slice(0,8)}-${hex.slice(8,12)}-${hex.slice(12,16)}-${hex.slice(16,20)}-${hex.slice(20)}`;
    }
    return 'cmd-' + Date.now().toString(36) + '-' + Math.random().toString(36).slice(2, 12);
  }
  function mount() {
    const $ = id => document.getElementById(id);
    const node = (tag, className, text) => {
      const result = document.createElement(tag);
      if (className) result.className = className;
      if (text !== undefined) result.textContent = text;
      return result;
    };
    const storage = window.RelaySession.safeStorage();
    let token = storage.get('relay-session') || '';
    const requestApi = window.RelaySession.createApi({ getToken: () => token });
    let states = [], state = null, connected = false, busy = false, uploading = false, refreshing = null, tab = 'workflow', signature = '';
    let setupReady = false, setupHint = '请先完成页面下方“接入设置”的检测。', shuttingDown = false, authExpired = false;
    const setup = window.RelaySetup.mount({ node, api, anchor: $('prototype-notice'),
      changed: (ready, hint) => { setupReady = ready; setupHint = hint || '请先完成页面下方“接入设置”的检测。'; render(); }, shutdown: () => { shuttingDown = true; connected = false; stopReads(); render(); } });
    const drafts = new Map();
    const phases = { draft: '准备需求', designing: '架构师生成设计中', document: '设计已生成，等待人工修改上传', uploaded: '已上传确认设计', splitting: '架构师按上传版本拆分中', review: '等待逐项审核', regenerating: '仅重做驳回任务中', approved: '全部通过，等待手动开工', developing: '开发者执行获批任务中', testing: '独立测试与可信复跑中', failed: '实际测试失败，等待返修', fixing: '开发者按实际缺陷返修中', ready: '实际测试通过，待人工验收', accepted: '人工验收已记录', paused: '当前阶段已结束，调度已暂停', stopping: '正在终止并确认进程树', stopped: '已终止，批准已撤销', blocked: '执行阻塞，禁止自动续跑', consulting: '只读咨询中，原工作不自动恢复' };
    document.body.dataset.mode = 'live';
    document.title = '接力 · 三 Claude 真实协作工作台';
    const setupLink = node('a', '', '接入设置'); setupLink.id = 'setup-link'; setupLink.href = '#setup-panel';
    $('prototype-notice').replaceChildren(node('span', '', '真实执行模式 · 生成、拆分、开发和测试会调用模型，可能计费；最后一项审核通过不会自动开工。'), setupLink);
    document.querySelector('.side-bottom').textContent = '真实本机模式 · 串行交接 · 非完整 OS 沙箱';
    document.querySelector('.outline-tag').textContent = 'P1 真实执行';
    document.querySelector('.workspace-label span').textContent = 'P1';
    document.querySelector('.side-project .mini-label').textContent = '当前受管理项目';
    document.querySelector('.map-transport p').textContent = '本机服务管批准、进程和实际证据；reason-network 传 TASK / ACK / RESULT。发送成功不等于模型已读，测试通过不等于人工验收。';
    document.querySelectorAll('.execute-lane .map-step strong')[0].textContent = 'Claude Code 开发者';
    document.querySelectorAll('.execute-lane .map-step strong')[1].textContent = '独立 Claude Code 测试员';
    document.querySelector('.runtime-panel .panel-heading > .subtle-tag').textContent = '真实进程与证据';
    document.querySelector('.runtime-status .mini-label').textContent = '后台权威状态';
    document.querySelector('.lease small').textContent = '受控工具租约，非 OS 沙箱';
    document.querySelector('footer span:last-child').textContent = 'P1 / 刷新保留后台状态 / 无静默模拟回退';
    document.querySelector('.transport-note p').textContent = '真实 reason：每次独立attempt有唯一邮箱和随机nonce；模型显式收件ACK后才开放项目工具。';
    $('file-help').textContent = '.md / .txt · 严格 UTF-8 · 最大256KiB。上传到当前本机服务，保留版本与SHA256；也可上传已有设计。';
    $('task-simulation-note').textContent = '真实架构师按上传文档拆分；逐项人工审核。驳回只重做该项，无关批准保留，依赖项显式重审。';
    const heading = document.querySelector('.page-heading h1'); heading.id = 'current-project-name';
    document.querySelector('.page-heading p').textContent = '选择项目，查看进展，再决定下一步。';
    const nav = document.querySelector('nav a'); nav.href = '#my-projects'; nav.textContent = '我的项目';
    const workflowHelp = node('details', 'workspace-help'); workflowHelp.id = 'workflow-help';
    workflowHelp.append(node('summary', '', '协作流程与岗位说明'), $('workflow-map'), document.querySelector('.roles'));
    $('delivery').after($('setup-panel'), workflowHelp);
    const toolbar = node('section', 'live-toolbar'); toolbar.id = 'my-projects'; toolbar.setAttribute('aria-labelledby', 'my-projects-title');
    const projectHeading = node('div', 'project-heading');
    const projectTitle = node('h2', '', '我的项目'); projectTitle.id = 'my-projects-title';
    const browse = node('button', 'button small', '浏览项目'); browse.id = 'my-projects-toggle'; browse.type = 'button'; browse.setAttribute('aria-expanded', 'false'); browse.setAttribute('aria-controls', 'project-browser');
    projectHeading.append(projectTitle, browse);
    const label = node('label', 'project-picker', '当前项目'); label.htmlFor = 'project-select';
    const select = node('select'); select.id = 'project-select'; label.append(select);
    const name = node('input'); name.id = 'new-project-name'; name.maxLength = 100; name.placeholder = '新项目名称'; name.setAttribute('aria-label', '新项目名称');
    const create = node('button', 'button small', '新建独立项目'); create.id = 'create-project'; create.type = 'button';
    const creation = node('div', 'project-create'); creation.append(name, create);
    const status = node('p', 'field-help', '连接中…'); status.id = 'connection-status'; status.setAttribute('role', 'status');
    const projectBrowser = node('div', 'project-browser'); projectBrowser.id = 'project-browser'; projectBrowser.hidden = true;
    const searchLabel = node('label', 'sr-only', '搜索当前工作台的项目'); searchLabel.htmlFor = 'project-search';
    const search = node('input'); search.id = 'project-search'; search.type = 'search'; search.placeholder = '搜索项目名称';
    const projectList = node('div', 'project-list'); projectList.id = 'project-list';
    projectBrowser.append(searchLabel, search, projectList);
    const storageWarning = node('p', 'field-help'); storageWarning.id = 'storage-warning'; storageWarning.setAttribute('role', 'status');
    toolbar.append(projectHeading, label, creation, status, storageWarning, projectBrowser);
    $('prototype-notice').after(toolbar);
    const progress = node('section', 'current-progress'); progress.id = 'current-progress'; progress.setAttribute('aria-labelledby', 'progress-title');
    const progressLabel = node('div', 'progress-label'); progressLabel.append(node('span', '', '当前进度'));
    const lastSync = node('span'); lastSync.id = 'last-sync'; progressLabel.append(lastSync);
    const progressTitle = node('h2'); progressTitle.id = 'progress-title';
    const progressDetail = node('p'); progressDetail.id = 'progress-detail';
    const progressNext = node('a', 'progress-next'); progressNext.id = 'progress-next';
    const progressFacts = node('dl', 'progress-facts'), progressValues = {};
    for (const [key, label] of [['agent', '当前 Agent'], ['task', '当前工作'], ['activity', '最近活动'], ['timing', '实际计时']]) {
      const row = node('div'), value = node('dd'); value.id = 'progress-' + key; progressValues[key] = value;
      row.append(node('dt', '', label), value); progressFacts.append(row);
    }
    const progressOutput = node('a', 'progress-next', '查看执行记录'); progressOutput.id = 'progress-output'; progressOutput.href = '#claude-panel';
    const progressActions = node('div', 'progress-actions'); progressActions.append(progressNext, progressOutput);
    progress.append(progressLabel, progressTitle, progressDetail, progressFacts, progressActions);
    toolbar.after(progress);
    workflowHelp.querySelector('summary').after(document.querySelector('.pipeline'));
    let syncedAt = null;
    const options = new Map(), projectRows = new Map();
    const projectEmpty = node('p', 'project-empty');
    browse.addEventListener('click', () => {
      projectBrowser.hidden = !projectBrowser.hidden; browse.setAttribute('aria-expanded', String(!projectBrowser.hidden));
      if (!projectBrowser.hidden) search.focus();
    });
    search.addEventListener('input', renderProjects);
    const save = node('button', 'button small', '保存需求'); save.id = 'save-requirement'; $('requirement').after(save);
    $('generate-document').textContent = '生成设计文档（真实 Claude）';
    const generateHint = node('p', 'field-help'); generateHint.id = 'generate-hint'; generateHint.setAttribute('role', 'status');
    $('generate-document').setAttribute('aria-describedby', generateHint.id);
    $('generate-document').closest('.requirement-actions').after(generateHint);
    $('split-tasks').textContent = '按上传版本拆分（真实 Claude）';
    $('start').textContent = '全部通过后，手动开始真实开发';
    const retest = node('button', 'button', '重新独立测试（真实 Claude）'); retest.id = 'retest'; $('fix').after(retest);
    const retry = node('button', 'button', '显式重试失败阶段（真实 Claude，可能计费）'); retry.id = 'retry'; retest.after(retry);
    $('fix').textContent = '按实际缺陷返修'; $('pause').textContent = '当前阶段结束后暂停';
    $('resume').textContent = '继续调度'; $('stop').textContent = '终止本轮进程与批准'; $('accept').textContent = '我已查看，确认人工验收';
    ['develop-complete', 'test-pass', 'test-fail', 'fix-complete', 'reset'].forEach(id => { $(id).hidden = true; $(id).disabled = true; });
    const delivery = document.querySelectorAll('.delivery-check')[1];
    delivery.replaceChildren(node('span', 'check-symbol', '▧'), node('div', '', '真实产物预览（独立来源）'));
    const previewButton = node('button', 'button small', '查看本轮产物'); previewButton.id = 'preview-product'; delivery.append(previewButton);
    const frame = node('iframe', 'live-preview'); frame.id = 'product-preview'; frame.title = '受隔离的本轮产品预览'; frame.setAttribute('sandbox', 'allow-scripts'); frame.referrerPolicy = 'no-referrer'; frame.hidden = true; $('delivery').append(frame);
    for (const role of ['architect', 'developer', 'tester']) { $('role-' + role).value = 'Claude Code'; $('role-' + role).disabled = true; }

    let observedProgress = null;
    const liveOutput = window.RelayOutput.mount({ node, api, observed: value => { observedProgress = value; renderProgress(); } });
    async function api(path, value, raw = false, options = {}) {
      try { return await requestApi(path, value, raw, options); }
      catch (error) { if (error.category === 'http' && error.status === 403) expireSession(); throw error; }
    }
    const agents = window.RelayAgents.mount({ node, api, storage, onState: value => {
      const known = states.find(item => item.id === value.id);
      if ((known?.revision || 0) > value.revision || state?.id === value.id && state.revision > value.revision) return;
      states = known ? states.map(item => item.id === value.id ? value : item) : [...states, value];
      if (state?.id === value.id) state = value;
      render();
    } });
    function stopReads() { statePoll.stop(); setupPoll.stop(); setup.stop(); liveOutput.stop(); agents.stop(); }
    function expireSession() {
      if (authExpired) return;
      authExpired = true; connected = false; setupReady = false; stopReads();
      status.textContent = '页面会话已失效，请通过原启动器重新打开认证页面；不会重放操作或重启后台任务。'; render();
    }
    function selectProject(id) {
      if (busy || uploading || !connected) return;
      const next = states.find(project => project.id === id); if (!next) return;
      state = next; storage.set('relay-project', state.id);
      frame.hidden = true; frame.removeAttribute('src'); render();
    }
    function renderProjects() {
      const currentIds = new Set(states.map(project => project.id));
      for (const [id, option] of options) if (!currentIds.has(id)) { option.remove(); options.delete(id); }
      for (const [id, row] of projectRows) if (!currentIds.has(id)) { row.remove(); projectRows.delete(id); }
      let shown = 0;
      const query = search.value.trim().toLocaleLowerCase();
      for (const project of states) {
        let option = options.get(project.id);
        if (!option) { option = node('option'); option.value = project.id; options.set(project.id, option); select.append(option); }
        const caption = project.name + ' · ' + project.id.slice(0, 8);
        if (option.textContent !== caption) option.textContent = caption;
        option.selected = project.id === state?.id;
        let row = projectRows.get(project.id);
        if (!row) {
          row = node('button', 'project-row'); row.type = 'button'; row.dataset.project = project.id;
          row.append(node('strong'), node('span'), node('small'));
          row.addEventListener('click', () => selectProject(project.id));
          projectRows.set(project.id, row); projectList.append(row);
        }
        row.querySelector('strong').textContent = project.name;
        row.querySelector('span').textContent = project.id.slice(0, 8) + ' · ' + (phases[project.phase] || '状态未知');
        const time = [...project.events].reverse().find(event => Number.isFinite(event.time) && event.time > 0)?.time;
        row.querySelector('small').textContent = time ? '最近活动 ' + new Date(time * 1000).toLocaleString('zh-CN') : '暂无活动记录';
        row.setAttribute('aria-current', String(project.id === state?.id));
        row.disabled = busy || uploading || !connected;
        row.hidden = !project.name.toLocaleLowerCase().includes(query);
        if (!row.hidden) shown++;
      }
      projectEmpty.textContent = states.length ? '没有匹配的项目；当前项目不会改变。' : '当前工作台还没有项目，请填写名称后新建。';
      projectEmpty.hidden = shown > 0;
      if (!projectEmpty.parentNode) projectList.append(projectEmpty);
      select.setAttribute('aria-label', connected ? '选择当前工作台的项目' : '最后已知项目，当前无法同步');
    }
    function renderProgress() {
      progress.dataset.stale = String(!connected);
      lastSync.textContent = syncedAt ? (connected ? '同步于 ' : '最后同步 ') + new Date(syncedAt).toLocaleTimeString('zh-CN') : '尚未同步';
      const value = window.RelayOutput.describeProgress(state, connected ? observedProgress : null);
      progressTitle.textContent = (connected ? '' : '最后已知：') + value.title;
      progressDetail.textContent = !connected ? '当前未同步，不能据此判断任务已经停止。' : value.detail;
      for (const key of Object.keys(progressValues)) progressValues[key].textContent = value[key];
      if (!connected) progressValues.agent.textContent = '后台未连接，Agent 当前状态无法核实。';
      progressNext.textContent = '下一步：' + value.next; progressNext.href = value.target;
    }
    function report(error) { status.textContent = '操作失败：' + error.message; }
    function refresh(options = {}) {
      if (shuttingDown || authExpired) return Promise.resolve();
      if (refreshing) return refreshing;
      refreshing = (async () => {
        const data = await api('/api/state', undefined, false, options);
        if (shuttingDown || authExpired || options.signal?.aborted) return;
        // A poll started before a command must never roll its response backwards.
        const known = new Map(states.map(item => [item.id, item]));
        if (state) known.set(state.id, state);
        states = data.projects.map(item => (known.get(item.id)?.revision || 0) > item.revision ? known.get(item.id) : item); connected = true; syncedAt = Date.now();
        const wanted = state?.id || storage.get('relay-project');
        state = states.find(p => p.id === wanted) || states[0] || null;
        const linuxBlocked = data.platform?.startsWith('linux-') && data.executionMode === 'blocked';
        status.textContent = linuxBlocked ? '已连接Linux服务器 · 执行器未验证，模型执行已禁用' : `已连接本机服务 · ${data.reason} · ${data.workers ? '真实执行器启用' : '测试模式：执行器未启动，不模拟模型结果'}`;
        if (linuxBlocked) {
          $('prototype-notice').replaceChildren(node('span', '', 'Linux控制台模式 · 项目和需求可以保存；执行器、原生依赖与浏览器沙箱未验证，模型操作禁用。'), setupLink);
          document.querySelector('.side-bottom').textContent = 'Linux服务器 · 仅控制台可用 · 非完整执行环境';
          document.querySelector('.outline-tag').textContent = 'Linux受限控制台';
        }
        render();
      })().catch(error => { stateFailure(error); throw error; }).finally(() => { refreshing = null; });
      return refreshing;
    }
    function stateFailure(error) {
      connected = false;
      if (!authExpired && !shuttingDown) status.textContent = '连接断开或读取失败：' + error.message;
      render();
    }
    const statePoll = window.RelaySession.createPoll({ read: refresh, success: () => {}, failure: () => {} });
    const setupPoll = window.RelaySession.createPoll({ read: options => setup.load(options), success: () => {}, failure: () => {} });
    async function command(kind, payload = {}) {
      if (!state || busy || !connected || kind !== 'stop' && agents.pending(state.id)) return;
      busy = true; render();
      try {
        state = await api(kind === 'upload' ? '/api/documents' : '/api/command', { projectId: state.id, commandId: makeCommandId(), expectedRevision: state.revision, kind, payload });
        await refresh();
      } catch (error) { await refresh().catch(() => {}); if (!authExpired) report(error); }
      finally { busy = false; render(); }
    }
    function taskCommand(kind, task, extra = {}) { return command(kind, { taskId: task.id, taskVersion: task.version, documentVersion: task.documentVersion, ...extra }); }
    function renderTasks(locked) {
      const nextSignature = JSON.stringify([state?.id, state?.tasks, locked, setupReady]);
      if (signature === nextSignature) return;
      signature = nextSignature;
      $('tasks').replaceChildren();
      if (!state?.tasks.length) { $('tasks').append(node('p', 'empty-state', '尚无任务：请先上传确认的设计，再由真实架构师拆分。')); return; }
      const labels = { pending: '待审核', approved: '已通过', rejected: '已驳回', 'needs-review': '需重审' };
      for (const task of state.tasks) {
        const card = node('article', 'task-row'); card.dataset.task = task.id; card.dataset.version = task.version; card.dataset.status = task.status;
        const meta = node('div', 'task-meta'); meta.append(node('code', 'task-id', `${task.id} / V${task.version} / U${task.documentVersion}`), node('span', 'task-owner', task.owner === 'developer' ? '开发者 · Claude' : '测试员 · 独立 Claude'), node('span', 'task-status', labels[task.status]));
        card.append(meta, node('h4', '', task.title), node('p', 'task-detail', task.detail));
        const list = node('ul', 'task-acceptance'); task.acceptance.forEach(point => list.append(node('li', '', point))); card.append(list);
        card.append(node('p', 'task-dependencies', '依赖：' + (task.dependsOn.join('、') || '无')));
        if (task.reviewReason || task.rejectionReason) card.append(node('p', 'review-reason', task.reviewReason || task.rejectionReason));
        const key = `${state.id}/${task.id}/${task.version}`;
        const reason = node('textarea'); reason.id = 'reason-' + task.id; reason.maxLength = 2000; reason.value = drafts.get(key) || ''; reason.disabled = locked;
        const reasonLabel = node('label', 'task-review-label', '驳回原因（必填）'); reasonLabel.htmlFor = reason.id;
        reason.addEventListener('input', () => { drafts.set(key, reason.value); });
        const buttons = node('div', 'task-buttons');
        for (const [kind, caption] of [['approve', '通过此任务'], ['reject', '驳回此任务'], ['regenerate', '仅重做此任务（真实 Claude）']]) {
          const button = node('button', 'button small', caption); button.dataset.action = kind;
          button.disabled = locked || (kind === 'regenerate' && !setupReady) || (kind === 'approve' ? !['pending', 'needs-review'].includes(task.status) : kind === 'regenerate' ? task.status !== 'rejected' : task.status === 'rejected');
          button.addEventListener('click', () => { if (kind === 'reject' && !reason.value.trim()) { reason.focus(); return; } taskCommand(kind, task, kind === 'reject' ? { reason: reason.value.trim() } : {}); });
          buttons.append(button);
        }
        const history = node('details', 'task-history'); history.append(node('summary', '', '审核与重做历史'));
        const lines = node('ul'); task.history.forEach(row => lines.append(node('li', '', `V${row.taskVersion} · ${row.kind} · ${row.message}`))); history.append(lines);
        card.append(reasonLabel, reason, buttons, history); $('tasks').append(card);
      }
    }
    function render() {
      storageWarning.hidden = storage.available;
      storageWarning.textContent = storage.available ? '' : '浏览器存储不可用：会话与选择仅留在此标签内存，刷新后无法恢复，请通过原启动器重新认证。';
      document.body.dataset.connected = String(connected); document.body.dataset.phase = state?.phase || 'draft'; document.body.dataset.project = state?.id || ''; document.body.dataset.uploading = String(busy || uploading);
      const projectName = state?.name || '未选择项目';
      document.querySelector('.side-project strong').textContent = projectName;
      document.querySelector('.side-project p').textContent = state ? '项目 ID · ' + state.id.slice(0, 8) : '请新建或选择当前工作台的项目';
      document.querySelector('.breadcrumb').replaceChildren(document.createTextNode('工作空间 '), node('span', '', '/'), document.createTextNode(' ' + projectName));
      document.querySelector('.project-badge').replaceChildren(node('span'), document.createTextNode(' ' + projectName));
      document.title = projectName + ' · 接力工作台';
      heading.textContent = projectName;
      const runtimeBusy = !!state?.activeAttempt || ['paused', 'stopping'].includes(state?.phase);
      const agentPending = !!(state && agents.pending(state.id));
      const locked = busy || uploading || agentPending || !connected || !state || runtimeBusy || state.leaseUnknown;
      renderProjects(); renderProgress();
      select.disabled = busy || uploading || !connected; name.disabled = create.disabled = busy || uploading || !connected;
      if (document.activeElement !== $('requirement')) $('requirement').value = state?.requirement || '';
      $('requirement').disabled = save.disabled = locked; $('document-file').disabled = locked;
      $('generate-document').disabled = locked || !setupReady; $('split-tasks').disabled = locked || !setupReady || !state?.uploadedDocument;
      generateHint.textContent = shuttingDown ? '工作台正在退出，不再接收生成请求。' :
        !connected ? '尚未连接或页面会话已失效。请双击 start-workbench.cmd 重新打开已认证页面，不要只输入本机地址；不会重启后台任务。' :
        !state ? '请先在页面上方填写项目名称，点击“新建独立项目”。' :
        busy || uploading ? '正在保存或读取文件，请等待完成。' :
        state.leaseUnknown ? '旧进程状态未知，禁止生成；请保留数据，不要删除锁或强行重试。' :
        runtimeBusy ? '当前项目仍有任务或调度尚未停稳，请先处理右侧执行状态。' :
        !setupReady ? setupHint : '可以生成设计；点击将调用真实模型，可能计费。';
      $('download-document').disabled = busy || !connected || !state?.generatedDocument;
      $('start').disabled = busy || uploading || agentPending || !connected || !setupReady || !state?.canStart;
      $('accept').disabled = busy || uploading || agentPending || !connected || state?.phase !== 'ready';
      retest.disabled = $('accept').disabled || !setupReady;
      retest.hidden = state?.phase !== 'ready';
      retry.hidden = state?.phase !== 'blocked' || !!state?.agentControl?.resumable;
      retry.disabled = busy || uploading || agentPending || !connected || !setupReady || state?.phase !== 'blocked' || state?.leaseUnknown || state?.recoveryRequired || !state?.lastAttempt || (state.lastAttempt.retryCount || 0) >= 2;
      for (const [id, phase] of [['fix', 'failed'], ['resume', 'paused']]) { $(id).hidden = state?.phase !== phase || id === 'resume' && !!state?.agentControl?.resumable; $(id).disabled = busy || agentPending || !connected || !setupReady; }
      $('pause').hidden = !['developing', 'testing', 'fixing'].includes(state?.phase); $('pause').disabled = busy || agentPending || !connected || state?.pauseRequested;
      $('stop').hidden = !state || ['draft', 'stopped', 'accepted'].includes(state.phase); $('stop').disabled = busy || !connected;
      $('generated-meta').textContent = state?.generatedDocument ? `G${state.generatedDocument.version} · 真实生成` : '尚未生成';
      $('generated-preview').textContent = state?.generatedDocument?.markdown || '';
      $('upload-preview').textContent = state?.uploadedDocument?.text || ''; $('uploaded-document').hidden = !state?.uploadedDocument;
      $('uploaded-meta').textContent = state?.uploadedDocument ? `U${state.uploadedDocument.version} · ${state.uploadedDocument.name} · SHA256 ${state.uploadedDocument.sha256}` : '尚未上传；生成与下载不代表批准。';
      $('revision').textContent = `需求 R${state?.contextRevision || 1} / 状态 ${state?.revision || 0}`;
      $('phase-title').textContent = phases[state?.phase] || '请新建独立项目';
      $('phase-description').textContent = state?.lastError || (state?.pauseRequested ? '已请求暂停；当前阶段仍在运行，结束后不再派工。' : '状态、版本、进程与测试结论由本机后台确认；不由页面伪造完成。');
      $('task-count').textContent = state?.tasks.length ? `${state.tasks.length} 项 / 已通过 ${state.tasks.filter(t => t.status === 'approved').length}` : '尚未拆分';
      $('approval-status').textContent = state?.canStart ? '全部当前版本已通过。现在仅解锁按钮，不自动开发。' : '任一任务待审、驳回、依赖需重审或旧进程未知时不能开工。';
      $('split-hint').textContent = state?.uploadedDocument ? '依据上传文档的不可变快照；重拆将清除已有批准。' : '先上传人工确认设计。';
      $('writer-status').textContent = state?.activeAttempt ? `唯一执行者：${state.activeAttempt.role} / ${state.activeAttempt.id.slice(0, 12)}` : state?.leaseUnknown ? '旧进程状态未知，禁止交接' : '本轮无活动写入者';
      for (const role of ['architect', 'developer', 'tester']) {
        const card = $('role-' + role).closest('.role-card');
        const last = [...(state?.executions || [])].reverse().find(row => row.memberId.includes('relay-' + role.slice(0, 4)));
        card.querySelector('.identity code').textContent = last ? `${last.memberId} / PID ${last.pid}` : '尚无实际进程记录';
        card.querySelector('.identity span').textContent = last ? '实际成员 / 已退出' : '每阶段独立启动';
        card.querySelector('.role-state').textContent = window.RelayAgents.describeRoles(state)[role].status;
      }
      $('workflow-count').textContent = state?.events.filter(e => e.channel === 'workflow').length || 0;
      $('reason-count').textContent = state?.events.filter(e => e.channel === 'reason').length || 0;
      $('event-log').replaceChildren(...(state?.events || []).filter(e => e.channel === tab).map(event => { const row = node('div', 'log-event'); row.append(node('code', '', String(event.number)), node('p', '', event.message)); return row; }));
      const evidence = state?.run?.evidence?.at(-1);
      $('test-summary').textContent = evidence ? `实际退出码 ${evidence.exitCode} / ${evidence.assertions} 条断言 / ${evidence.id}；${evidence.rerunId ? '可信复跑已完成' : ''}` : '尚无实际浏览器测试证据；模型自报不算通过。';
      $('test-symbol').textContent = evidence ? evidence.exitCode === 0 ? '✓' : '×' : '○';
      $('delivery-status').textContent = phases[state?.phase] || '未开始';
      previewButton.disabled = !state?.executions?.some(e => e.productManifest?.files?.length);
      const position = { draft: 0, designing: 1, document: 2, uploaded: 2, splitting: 3, review: 3, regenerating: 3, approved: 3, developing: 4, fixing: 4, testing: 5, failed: 5, ready: 6, accepted: 6 }[state?.phase] || 0;
      document.querySelectorAll('.pipeline li').forEach((li, index) => { li.classList.toggle('current', index === position); li.classList.toggle('done', index < position); });
      liveOutput.setProject(state, connected && !shuttingDown);
      agents.setProject(state, connected && !shuttingDown, setupReady && !busy && !uploading);
      renderTasks(locked || !['review', 'approved'].includes(state?.phase));
    }
    save.addEventListener('click', () => command('edit', { requirement: $('requirement').value }));
    $('generate-document').addEventListener('click', async () => { if ($('requirement').value !== state.requirement) { await command('edit', { requirement: $('requirement').value }); if ($('requirement').value !== state.requirement) return; } command('generate'); });
    create.addEventListener('click', async () => { if (!name.value.trim() || busy) return; busy = true; render(); try { state = await api('/api/projects', { name: name.value.trim() }); storage.set('relay-project', state.id); name.value = ''; await refresh(); } catch (error) { report(error); } finally { busy = false; render(); } });
    select.addEventListener('change', () => selectProject(select.value));
    $('document-file').addEventListener('change', async () => {
      const file = $('document-file').files[0]; if (!file || uploading || busy) return;
      const project = state?.id, revision = state?.revision;
      uploading = true; $('upload-error').textContent = ''; render();
      try {
        const source = file.size > 256 * 1024 ? file.slice(0, 256 * 1024 + 1) : file;
        const bytes = await source.arrayBuffer();
        const result = window.WorkbenchModel.validateDocumentFile(file.name, new Uint8Array(bytes));
        if (!result.ok) throw Error(result.error);
        if (project !== state?.id || revision !== state?.revision) throw Error('项目版本已变化，请重新选择文件');
        await command('upload', { name: file.name, text: result.text });
      } catch (error) { $('upload-error').textContent = error.message; }
      finally { uploading = false; $('document-file').value = ''; render(); }
    });
    $('download-document').addEventListener('click', async () => { try { const blob = await api('/api/document?project=' + state.id, undefined, true); const url = URL.createObjectURL(blob); const a = node('a'); a.href = url; a.download = 'design.md'; a.click(); setTimeout(() => URL.revokeObjectURL(url), 1000); } catch (error) { report(error); } });
    $('split-tasks').addEventListener('click', () => { const confirmed = !state.tasks.length || confirm('重新拆分会清空原任务和逐项批准，确认继续？'); if (confirmed) command('split', { confirmed: true }); });
    retry.addEventListener('click', () => { if (confirm('仅重试失败阶段，保留当前批准和已完成任务；会再次调用真实 Claude，可能计费。确认重试？')) command('retry'); });
    for (const [id, kind] of [['retest', 'retest'], ['start', 'start'], ['pause', 'pause'], ['resume', 'resume'], ['stop', 'stop'], ['fix', 'repair'], ['accept', 'accept']]) $(id).addEventListener('click', () => command(kind));
    function setTab(value) {
      tab = value;
      for (const key of ['workflow', 'reason', 'claude']) {
        $('tab-' + key).setAttribute('aria-selected', String(value === key));
        $('tab-' + key).tabIndex = value === key ? 0 : -1;
      }
      $('event-log').hidden = value === 'claude'; liveOutput.visible(value === 'claude');
      $('event-log').setAttribute('aria-labelledby', 'tab-' + value); render();
    }
    progressOutput.addEventListener('click', () => setTab('claude'));
    for (const value of ['workflow', 'reason', 'claude']) {
      $('tab-' + value).addEventListener('click', () => setTab(value));
      $('tab-' + value).addEventListener('keydown', event => {
        if (!['ArrowLeft', 'ArrowRight', 'Home', 'End'].includes(event.key)) return;
        event.preventDefault();
        const tabs = ['workflow', 'reason', 'claude'];
        setTab(event.key === 'Home' ? tabs[0] : event.key === 'End' ? tabs[2] : tabs[(tabs.indexOf(tab) + (event.key === 'ArrowRight' ? 1 : 2)) % 3]);
        $('tab-' + tab).focus();
      });
    }
    previewButton.addEventListener('click', () => { frame.src = state.previewUrl; frame.hidden = false; });
    // 可用性增强：禁用按钮无法触发 click，捕获阶段拦截指针落点，给出明确的拒绝原因，避免“点了没反应”。
    const toast = node('div', 'disabled-toast'); toast.id = 'disabled-toast'; toast.setAttribute('role', 'alert'); toast.hidden = true;
    document.body.append(toast);
    let toastTimer = null;
    function explainDisabled(button) {
      const hint = button.getAttribute('aria-describedby')?.split(/\s+/).map(id => document.getElementById(id))
        .find(element => element?.textContent?.trim()) || button.parentElement?.querySelector('.field-help, [id$="-hint"]');
      const reason = (hint?.textContent || '').trim() || '当前状态不满足该操作的启用条件。';
      toast.textContent = `［无法操作］${button.textContent.trim()}：${reason}`;
      toast.hidden = false;
      clearTimeout(toastTimer);
      toastTimer = setTimeout(() => { toast.hidden = true; }, 6000);
      if (hint) {
        hint.scrollIntoView({ block: 'center', behavior: 'smooth' });
        hint.animate?.([{ backgroundColor: 'rgba(255,196,66,.45)' }, { backgroundColor: 'transparent' }], { duration: 1800 });
      }
    }
    document.addEventListener('pointerdown', event => {
      // Chrome 不给 disabled button 派发 click，pointerdown 仍会触发；在此给出明确原因。
      if (authExpired || shuttingDown) return;
      const target = event.target instanceof Element ? event.target : null;
      const button = target?.closest?.('button');
      if (button?.disabled) { event.preventDefault(); explainDisabled(button); }
    }, true);
    window.addEventListener('pagehide', () => { shuttingDown = true; stopReads(); clearTimeout(toastTimer); });
    render();
    (async () => {
      const bootstrap = new URLSearchParams(location.hash.slice(1)).get('bootstrap');
      history.replaceState(null, '', location.pathname);
      try {
        if (bootstrap) { token = (await api('/api/session', { bootstrap })).token; storage.set('relay-session', token); }
        if (!authExpired && !shuttingDown) { statePoll.start(); setupPoll.start(); }
      } catch (error) { connected = false; if (!authExpired) report(error); render(); }
    })();
  }
})();
