(function () {
  'use strict';
  // Inert in file:// mode. The live controller never calls model.transition().
  window.RelayLive = { mount };
  function mount() {
    const $ = id => document.getElementById(id);
    const node = (tag, className, text) => {
      const result = document.createElement(tag);
      if (className) result.className = className;
      if (text !== undefined) result.textContent = text;
      return result;
    };
    let token = sessionStorage.getItem('relay-session') || '';
    let states = [], state = null, connected = false, busy = false, uploading = false, refreshing = false, tab = 'workflow', signature = '';
    let setupReady = false, setupHint = '请先完成页面上方的首次接入检测。', shuttingDown = false;
    const setup = window.RelaySetup.mount({ node, api, anchor: $('prototype-notice'),
      changed: (ready, hint) => { setupReady = ready; setupHint = hint || '请先完成页面上方的首次接入检测。'; render(); }, shutdown: () => { shuttingDown = true; connected = false; render(); } });
    const drafts = new Map();
    const phases = { draft: '准备需求', designing: '架构师生成设计中', document: '设计已生成，等待人工修改上传', uploaded: '已上传确认设计', splitting: '架构师按上传版本拆分中', review: '等待逐项审核', regenerating: '仅重做驳回任务中', approved: '全部通过，等待手动开工', developing: '开发者执行获批任务中', testing: '独立测试与可信复跑中', failed: '实际测试失败，等待返修', fixing: '开发者按实际缺陷返修中', ready: '实际测试通过，待人工验收', accepted: '人工验收已记录', paused: '当前阶段已结束，调度已暂停', stopping: '正在终止并确认进程树', stopped: '已终止，批准已撤销', blocked: '执行阻塞，禁止自动续跑' };
    document.body.dataset.mode = 'live';
    document.title = '接力 · 三 Claude 真实协作工作台';
    $('prototype-notice').textContent = '真实执行模式 · 三个独立 Claude Code 进程；reason-network 真实通信。生成、拆分、重做、开发和测试会调用模型，可能计费。最后一项审核通过不会自动开工。';
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
    const toolbar = node('section', 'live-toolbar');
    const label = node('label', '', '受管理项目 ');
    const select = node('select'); select.id = 'project-select'; label.append(select);
    const name = node('input'); name.id = 'new-project-name'; name.maxLength = 100; name.placeholder = '新项目名称'; name.setAttribute('aria-label', '新项目名称');
    const create = node('button', 'button small', '新建独立项目'); create.id = 'create-project';
    const status = node('p', 'field-help', '连接中…'); status.id = 'connection-status'; status.setAttribute('role', 'status');
    toolbar.append(label, name, create, status);
    $('prototype-notice').after(toolbar);
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

    const liveOutput = window.RelayOutput.mount({ node, api });
    async function api(path, value, raw = false, options = {}) {
      const response = await fetch(path, { method: value === undefined ? 'GET' : 'POST', cache: 'no-store', credentials: 'omit',
        headers: { ...(value === undefined ? {} : { 'Content-Type': 'application/json' }), ...(token ? { Authorization: 'Bearer ' + token } : {}) },
        body: value === undefined ? undefined : JSON.stringify(value), signal: options.signal });
      if (raw && response.ok) return response.blob();
      const body = await response.json();
      if (!response.ok) throw Error(body.error || '本机请求失败');
      return body;
    }
    function report(error) { status.textContent = '操作失败：' + error.message; }
    async function refresh() {
      if (refreshing || shuttingDown) return;
      refreshing = true;
      try {
        const data = await api('/api/state');
        await setup.load();
        // A poll started before a command must never roll its response backwards.
        const known = new Map(states.map(item => [item.id, item]));
        if (state) known.set(state.id, state);
        states = data.projects.map(item => (known.get(item.id)?.revision || 0) > item.revision ? known.get(item.id) : item); connected = true;
        const wanted = state?.id || sessionStorage.getItem('relay-project');
        state = states.find(p => p.id === wanted) || states[0] || null;
        status.textContent = `已连接本机服务 · ${data.reason} · ${data.workers ? '真实执行器启用' : '测试模式：执行器未启动，不模拟模型结果'}`;
      } catch (error) { connected = false; status.textContent = '连接断开或认证失败：' + error.message; }
      finally { refreshing = false; }
      render();
    }
    async function command(kind, payload = {}) {
      if (!state || busy || !connected) return;
      busy = true; render();
      try {
        state = await api(kind === 'upload' ? '/api/documents' : '/api/command', { projectId: state.id, commandId: crypto.randomUUID(), expectedRevision: state.revision, kind, payload });
        await refresh();
      } catch (error) { await refresh(); report(error); }
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
      document.body.dataset.connected = String(connected); document.body.dataset.phase = state?.phase || 'draft'; document.body.dataset.project = state?.id || ''; document.body.dataset.uploading = String(busy || uploading);
      const runtimeBusy = !!state?.activeAttempt || ['paused', 'stopping'].includes(state?.phase);
      const locked = busy || uploading || !connected || !state || runtimeBusy || state.leaseUnknown;
      select.replaceChildren(...states.map(item => { const option = node('option', '', item.name); option.value = item.id; option.selected = item.id === state?.id; return option; }));
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
      $('start').disabled = busy || uploading || !connected || !setupReady || !state?.canStart;
      $('accept').disabled = busy || uploading || !connected || state?.phase !== 'ready';
      retest.disabled = $('accept').disabled || !setupReady;
      retest.hidden = state?.phase !== 'ready';
      retry.hidden = state?.phase !== 'blocked';
      retry.disabled = busy || uploading || !connected || !setupReady || state?.phase !== 'blocked' || state?.leaseUnknown || state?.recoveryRequired || !state?.lastAttempt || (state.lastAttempt.retryCount || 0) >= 2;
      for (const [id, phase] of [['fix', 'failed'], ['resume', 'paused']]) { $(id).hidden = state?.phase !== phase; $(id).disabled = busy || !connected || !setupReady; }
      $('pause').hidden = !['developing', 'testing', 'fixing'].includes(state?.phase); $('pause').disabled = busy || !connected || state?.pauseRequested;
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
        card.querySelector('.role-state').textContent = state?.activeAttempt?.role === role ? '执行中' : '独立岗位';
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
      renderTasks(locked || !['review', 'approved'].includes(state?.phase));
    }
    save.addEventListener('click', () => command('edit', { requirement: $('requirement').value }));
    $('generate-document').addEventListener('click', async () => { if ($('requirement').value !== state.requirement) { await command('edit', { requirement: $('requirement').value }); if ($('requirement').value !== state.requirement) return; } command('generate'); });
    create.addEventListener('click', async () => { if (!name.value.trim() || busy) return; busy = true; render(); try { state = await api('/api/projects', { name: name.value.trim() }); sessionStorage.setItem('relay-project', state.id); name.value = ''; await refresh(); } catch (error) { report(error); } finally { busy = false; render(); } });
    select.addEventListener('change', () => { state = states.find(p => p.id === select.value); sessionStorage.setItem('relay-project', state.id); frame.hidden = true; frame.removeAttribute('src'); render(); });
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
    render();
    (async () => {
      const bootstrap = new URLSearchParams(location.hash.slice(1)).get('bootstrap');
      history.replaceState(null, '', location.pathname);
      try { if (bootstrap) { token = (await api('/api/session', { bootstrap })).token; sessionStorage.setItem('relay-session', token); } await setup.load(); await refresh(); }
      catch (error) { connected = false; report(error); render(); }
      setInterval(refresh, 1200);
    })();
  }
})();
