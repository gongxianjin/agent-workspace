(function () {
  'use strict';
  if (location.protocol !== 'file:') {
    if (window.RelayLive) window.RelayLive.mount();
    else {
      document.getElementById('prototype-notice').textContent = '真实模式加载失败；不会回退为模拟。请检查本机服务。';
      document.querySelectorAll('button, input, textarea, select').forEach(node => { node.disabled = true; });
    }
    return;
  }
  const model = window.WorkbenchModel;
  let state = model.createState();
  let tab = 'workflow';
  const drafts = new Map();
  const $ = id => document.getElementById(id);
  const planningPhases = ['draft', 'document', 'uploaded', 'review', 'approved', 'stopped', 'ready', 'accepted'];
  const phaseInfo = {
    draft: ['准备项目需求', '先生成设计，人工修改上传后再拆任务；也可以上传已有设计。'],
    document: ['设计文档已准备，等待你修改上传', '生成示例不等于确认设计；下载后修改，再上传有效版本。'],
    uploaded: ['已收到上传文档，等待拆分', '本轮只模拟拆分，预制任务将关联你刚上传的文档版本。'],
    review: ['等待逐项审核', '通过的保留；驳回时说明原因，仅重做该项后再次审核。'],
    approved: ['全部任务通过，等待你开始', '门禁已解锁，但不会自动启动开发或任何AI。'],
    developing: ['开发者正在接力 · 模拟', '仅演示全部任务通过后的串行开发；点击下方按钮推进。'],
    testing: ['独立测试阶段 · 模拟', '选择模拟测试结果，体验正常交付或返修复测。'],
    failed: ['模拟测试失败，需要修复', '验收已被阻止，交回开发者修复后必须重新测试。'],
    fixing: ['开发者修复中 · 模拟', '仅限已批准范围，不跳过复测和人工验收。'],
    ready: ['模拟测试通过，等待验收', '可查看预制示例；没有运行真实BoBot测试。'],
    accepted: ['模拟验收完成', '操作流程可走通，不代表三个真实AI已经完成交付。'],
    paused: ['前端演示已暂停', '只影响页面状态，没有后台进程在运行。'],
    stopped: ['前端演示已终止', '批准已撤销；重新上传或重新拆分并逐项审核，也可重置演示。']
  };
  const roleLabels = { architect: '架构师', developer: '开发者', tester: '测试员' };
  const statusLabels = { pending: '待审核', approved: '已通过', rejected: '已驳回', 'needs-review': '需要重审' };
  function element(tag, className, text) {
    const node = document.createElement(tag);
    if (className) node.className = className;
    if (text !== undefined) node.textContent = text;
    return node;
  }
  function dispatch(type, data = {}) {
    const next = model.transition(state, { type, token: model.token(state), ...data });
    if (next === state) return false;
    state = next;
    render();
    return true;
  }
  function renderTasks() {
    const signature = JSON.stringify([state.epoch, state.tasks, state.roles, state.phase, !!state.pendingUpload]);
    if ($('tasks').dataset.signature === signature) return;
    $('tasks').dataset.signature = signature;
    $('tasks').replaceChildren();
    if (!state.tasks.length) {
      const empty = element('div', 'empty-state');
      empty.append(element('span', '', '▤'), element('p', '', '先确认设计，再审核任务'),
        element('small', '', '上传有效文档 → 模拟拆分 → 逐项通过或驳回。'));
      $('tasks').append(empty);
      return;
    }
    const available = ['review', 'approved'].includes(state.phase) && !state.pendingUpload;
    for (const item of state.tasks) {
      const row = element('article', 'task-row');
      row.dataset.task = item.id;
      row.dataset.version = item.version;
      row.dataset.status = item.status;
      const meta = element('div', 'task-meta');
      meta.append(element('span', 'task-id', `${item.id} · v${item.version} · U${item.documentVersion}`),
        element('span', 'task-owner', `${roleLabels[item.owner]} · ${state.roles[item.owner]}`),
        element('span', 'task-status', statusLabels[item.status]));
      const heading = element('h4', '', item.title);
      heading.tabIndex = -1;
      const acceptance = element('ul', 'task-acceptance');
      for (const point of item.acceptance) acceptance.append(element('li', '', point));
      row.append(meta, heading, element('p', 'task-detail', item.detail), acceptance,
        element('p', 'task-dependencies', '依赖：' + (item.dependsOn.length ? item.dependsOn.join('、') : '无（独立任务）')));
      if (item.reviewReason) row.append(element('p', 'review-reason', item.reviewReason));
      if (item.rejectionReason) row.append(element('p', 'review-reason', '最近驳回原因：' + item.rejectionReason));
      const key = `${state.epoch}:${item.documentVersion}:${item.id}:${item.version}`;
      const label = element('label', 'task-review-label', '不合理的地方是什么？驳回时必填');
      label.htmlFor = 'reason-' + item.id;
      const reason = document.createElement('textarea');
      reason.id = label.htmlFor;
      reason.rows = 2;
      reason.maxLength = 2000;
      reason.placeholder = '例如：缺少空列表验收条件，请补充后再审。';
      reason.value = drafts.has(key) ? drafts.get(key) : '';
      reason.disabled = !available || item.status === 'rejected';
      const controls = element('div', 'task-buttons');
      const approve = element('button', 'button small primary', '✓ 通过此任务');
      const reject = element('button', 'button small danger-outline', '驳回此任务');
      const regenerate = element('button', 'button small', '↶ 仅重做此任务（模拟）');
      const dependencyBlocked = item.dependsOn.some(id => {
        const dependency = state.tasks.find(task => task.id === id);
        return !dependency || ['rejected', 'needs-review'].includes(dependency.status);
      });
      approve.disabled = !available || !['pending', 'needs-review'].includes(item.status) || dependencyBlocked;
      reject.disabled = !available || item.status === 'rejected' || !reason.value.trim();
      regenerate.hidden = item.status !== 'rejected';
      regenerate.disabled = !available || item.status !== 'rejected';
      const send = type => {
        if (dispatch(type, { task: item.id, version: item.version, documentVersion: item.documentVersion, reason: reason.value })) {
          const current = $('tasks').querySelector(`[data-task="${item.id}"] h4`);
          if (current) current.focus({ preventScroll: true });
        }
      };
      for (const [button, name, type] of [[approve, 'approve', 'approve-task'], [reject, 'reject', 'reject-task'], [regenerate, 'regenerate', 'regenerate-task']]) {
        button.type = 'button';
        button.dataset.action = name;
        button.addEventListener('click', () => send(type));
      }
      reason.addEventListener('input', () => {
        drafts.set(key, reason.value);
        reject.disabled = !available || item.status === 'rejected' || !reason.value.trim();
      });
      controls.append(approve, reject, regenerate);
      const history = element('details', 'task-history');
      const list = element('ul');
      for (const entry of item.history) list.append(element('li', '', `v${entry.version} / U${entry.documentVersion} · ${entry.message}`));
      history.append(element('summary', '', `审核与重做历史 · ${item.history.length} 条`), list);
      row.append(label, reason, controls, history);
      $('tasks').append(row);
    }
  }
  function renderLog() {
    $('event-log').replaceChildren();
    const events = state.events.filter(event => event.channel === tab);
    $('workflow-count').textContent = state.events.filter(event => event.channel === 'workflow').length;
    $('reason-count').textContent = state.events.filter(event => event.channel === 'reason').length;
    for (const name of ['workflow', 'reason']) {
      const active = name === tab;
      $('tab-' + name).setAttribute('aria-selected', String(active));
      $('tab-' + name).tabIndex = active ? 0 : -1;
    }
    $('event-log').setAttribute('aria-labelledby', 'tab-' + tab);
    if (!events.length) {
      const empty = element('div', 'log-placeholder');
      empty.append(element('span', '', '⌁'), element('p', '', tab === 'reason' ? '暂时没有模拟reason消息' : '你的文档与审核操作将在这里记录'),
        element('small', '', '没有真实任务发送，也没有AI收件确认。'));
      $('event-log').append(empty);
      return;
    }
    for (const event of events) {
      const row = element('div', 'log-event');
      const content = element('div');
      const message = element('p');
      message.append(element('span', 'log-badge', tab === 'reason' ? '模拟' : '原型'), document.createTextNode(event.message));
      content.append(element('small', '', `${event.from} → ${event.to}`), message);
      row.append(element('code', '', String(event.number).padStart(2, '0')), content);
      $('event-log').append(row);
    }
    $('event-log').scrollTop = $('event-log').scrollHeight;
  }
  function showButton(id, available, keepVisible = false) {
    $(id).hidden = !available && !keepVisible;
    $(id).disabled = !available;
  }
  function render() {
    document.body.dataset.phase = state.phase;
    document.body.dataset.uploading = String(!!state.pendingUpload);
    if ($('requirement').value !== state.requirement) $('requirement').value = state.requirement;
    for (const role of Object.keys(state.roles)) $('role-' + role).value = state.roles[role];
    const planning = planningPhases.includes(state.phase);
    $('revision').textContent = `需求 R${state.revision}`;
    $('phase-title').textContent = state.pendingUpload ? '正在读取设计文件' : phaseInfo[state.phase][0];
    $('phase-description').textContent = state.pendingUpload ? '读取完成前暂停任务拆分、审核与执行；旧有效版本暂时保留。' : phaseInfo[state.phase][1];
    $('generate-document').disabled = !planning || !state.requirement.trim();
    $('download-document').disabled = !state.generatedDocument;
    $('generated-document').hidden = !state.generatedDocument;
    $('generated-meta').textContent = state.generatedDocument ? `示例 G${state.generatedDocument.version} · 需求 R${state.generatedDocument.revision}` : '尚未生成';
    $('generated-preview').textContent = state.generatedDocument ? state.generatedDocument.text : '';
    $('document-file').disabled = !planning || !state.requirement.trim();
    $('upload-error').textContent = state.uploadError;
    $('uploaded-document').hidden = !state.uploadedDocument;
    $('upload-preview').textContent = state.uploadedDocument ? state.uploadedDocument.text : '';
    $('uploaded-meta').textContent = state.pendingUpload ? '正在本页读取文件，尚未替换上一有效版本…' : state.uploadedDocument ?
      `U${state.uploadedDocument.version} · ${state.uploadedDocument.name} · 已接收本页快照，未上传服务器` : '尚未上传；下载或生成设计不能解锁任务拆分。';
    $('split-tasks').disabled = !planning || !state.uploadedDocument || !!state.pendingUpload;
    $('split-tasks').textContent = state.tasks.length ? '重新拆分整份清单（模拟）' : '拆分任务（模拟）';
    $('split-hint').textContent = state.pendingUpload ? '等待读取完成。' : state.tasks.length ? '会清空现有任务与批准，操作前需要确认。' : state.uploadedDocument ? `仅关联上传文档 U${state.uploadedDocument.version}；不会理解正文。` : '先上传有效设计文档。';
    const approved = state.tasks.filter(item => item.status === 'approved').length;
    $('task-count').textContent = state.tasks.length ? `${approved} / ${state.tasks.length} 项已通过` : '尚未拆分';
    const allApproved = state.tasks.length && approved === state.tasks.length;
    $('approval-title').textContent = allApproved ? '当前版本全部通过' : '全部任务通过，才能开始开发';
    $('approval-status').textContent = state.pendingUpload ? '正在读取新文件，门禁临时锁定；失败会保留原批准。' : allApproved ?
      '仅解锁手动开始，不会自动运行AI。新上传、重拆或驳回将重新锁定。' : '任何一项待审、驳回或需要重审，都不能开工。';
    renderTasks();
    showButton('start', model.canStart(state), true);
    showButton('develop-complete', state.phase === 'developing');
    showButton('test-pass', state.phase === 'testing');
    showButton('test-fail', state.phase === 'testing');
    showButton('fix', state.phase === 'failed');
    showButton('fix-complete', state.phase === 'fixing');
    showButton('pause', ['developing', 'testing', 'fixing'].includes(state.phase));
    showButton('resume', state.phase === 'paused');
    showButton('stop', !['draft', 'stopped', 'accepted'].includes(state.phase) || !!state.pendingUpload);
    $('accept').disabled = state.phase !== 'ready' || !!state.pendingUpload;
    const writer = ['developing', 'fixing'].includes(state.phase) ? '开发者' : state.phase === 'testing' ? '测试员（测试范围）' : '未分配';
    $('writer-status').textContent = '示例写入权：' + writer;
    const effective = state.phase === 'paused' ? state.pausedPhase : state.phase;
    const phaseIndex = { draft: 0, document: 1, uploaded: 2, review: 3, approved: 3, developing: 4, fixing: 4, testing: 5, failed: 5, ready: 6, accepted: 6, stopped: -1 }[effective];
    document.querySelectorAll('.pipeline li').forEach((node, index) => {
      node.classList.toggle('current', index === phaseIndex);
      node.classList.toggle('done', index < phaseIndex);
      if (index === phaseIndex) node.setAttribute('aria-current', 'step');
      else node.removeAttribute('aria-current');
    });
    const passed = ['ready', 'accepted'].includes(state.phase);
    $('test-symbol').textContent = passed ? '✓' : state.phase === 'failed' ? '!' : '○';
    $('test-summary').textContent = passed ? '模拟通过；不是BoBot的真实测试结果。' : state.phase === 'failed' ? '模拟失败；先修复，再复测。' : '尚无模拟通过结果；未运行BoBot。';
    $('delivery-status').textContent = state.phase === 'accepted' ? '已完成模拟验收' : passed ? '等待人工模拟验收' :
      ({ developing: '开发演示中', fixing: '返修演示中', testing: '测试演示中', failed: '模拟测试失败', paused: '演示已暂停', stopped: '演示已终止' }[state.phase] || '尚未开始演示');
    renderLog();
  }
  $('requirement').addEventListener('input', event => dispatch('edit', { field: 'requirement', value: event.target.value }));
  for (const role of Object.keys(roleLabels)) $('role-' + role).addEventListener('change', event => dispatch('edit', { field: 'role', role, value: event.target.value }));
  $('generate-document').addEventListener('click', () => {
    if ((state.tasks.length || state.uploadedDocument) && !window.confirm('重新生成设计将清空上传文档、任务及批准。确定继续？')) return;
    if (dispatch('generate-document')) $('generated-document').open = true;
  });
  $('download-document').addEventListener('click', () => {
    if (!state.generatedDocument) return;
    const url = URL.createObjectURL(new Blob([state.generatedDocument.text], { type: 'text/markdown;charset=utf-8' }));
    const link = document.createElement('a');
    link.href = url;
    link.download = `relay-design-G${state.generatedDocument.version}.md`;
    link.click();
    setTimeout(() => URL.revokeObjectURL(url), 1000);
  });
  $('document-file').addEventListener('change', async event => {
    const file = event.target.files[0];
    event.target.value = '';
    if (!file || !dispatch('upload-begin')) return;
    const ticket = { ...state.pendingUpload };
    try {
      // Read only enough bytes to reject oversized files, not an unbounded user-selected file.
      const source = file.size > 256 * 1024 ? file.slice(0, 256 * 1024 + 1) : file;
      const bytes = new Uint8Array(await source.arrayBuffer());
      const oldVersion = state.uploadedDocument && state.uploadedDocument.version;
      if (dispatch('upload-success', { ticket, name: file.name, bytes }) && state.uploadedDocument && state.uploadedDocument.version !== oldVersion) {
        $('uploaded-document').open = true;
      }
    } catch (_) { dispatch('upload-failure', { ticket }); }
  });
  $('split-tasks').addEventListener('click', () => {
    const confirmed = state.tasks.length > 0;
    if (confirmed && !window.confirm('重新拆分后，旧任务清单及逐项批准将被清空。确定继续？')) return;
    dispatch('split-tasks', { confirmed });
  });
  for (const name of ['start', 'develop-complete', 'test-pass', 'test-fail', 'fix', 'fix-complete', 'pause', 'resume', 'stop', 'reset', 'accept']) {
    $(name).addEventListener('click', () => { if (name === 'reset') drafts.clear(); dispatch(name); });
  }
  for (const name of ['workflow', 'reason']) {
    $('tab-' + name).addEventListener('click', () => { tab = name; renderLog(); });
    $('tab-' + name).addEventListener('keydown', event => {
      if (['ArrowLeft', 'ArrowRight', 'Home', 'End'].includes(event.key)) {
        event.preventDefault();
        tab = event.key === 'Home' ? 'workflow' : event.key === 'End' ? 'reason' : tab === 'workflow' ? 'reason' : 'workflow';
        renderLog();
        $('tab-' + tab).focus();
      }
    });
  }
  render();
})();
