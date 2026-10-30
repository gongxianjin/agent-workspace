(function (root, factory) {
  if (typeof module === 'object' && module.exports) module.exports = factory();
  else root.RelayAgents = factory();
})(typeof globalThis === 'object' ? globalThis : this, function () {
  'use strict';
  const roles = { architect: '架构师', developer: '开发者', tester: '测试员' };
  const kinds = ['agent_message', 'agent_resolve', 'agent_handoff', 'agent_consult', 'agent_resume'];
  const storageKey = 'relay-agent-pending';
  // 幂等 commandId：安全上下文用 randomUUID，普通 HTTP 部署回退（与 live.js 的 makeCommandId 同策略）。
  function randomIdFallback() {
    if (typeof crypto !== 'undefined' && typeof crypto.randomUUID === 'function') return crypto.randomUUID();
    if (typeof crypto !== 'undefined' && crypto.getRandomValues) {
      const bytes = crypto.getRandomValues(new Uint8Array(16));
      bytes[6] = bytes[6] & 0x0f | 0x40; bytes[8] = bytes[8] & 0x3f | 0x80;
      const hex = [...bytes].map(b => b.toString(16).padStart(2, '0')).join('');
      return `${hex.slice(0,8)}-${hex.slice(8,12)}-${hex.slice(12,16)}-${hex.slice(16,20)}-${hex.slice(20)}`;
    }
    return 'cmd-' + Date.now().toString(36) + '-' + Math.random().toString(36).slice(2, 12);
  }
  function createCommands({ api, storage, onState, onChange, makeId = randomIdFallback, now = () => Date.now() }) {
    const pending = new Map(), reads = new Map(), revisions = new Map();
    let stopped = false;
    const fields = ['projectId', 'role', 'commandId', 'kind', 'expectedRevision', 'time'];
    function valid(item) {
      return item && Object.keys(item).length === fields.length && fields.every(key => Object.hasOwn(item, key)) &&
        /^[a-f0-9]{32}$/.test(item.projectId) && Object.hasOwn(roles, item.role) && kinds.includes(item.kind) &&
        typeof item.commandId === 'string' && /^[A-Za-z0-9_-]{1,100}$/.test(item.commandId) &&
        Number.isSafeInteger(item.expectedRevision) && item.expectedRevision >= 0 && Number.isSafeInteger(item.time) && item.time > 0;
    }
    try {
      const saved = JSON.parse(storage.get(storageKey) || '[]');
      if (Array.isArray(saved) && saved.length <= 50) for (const item of saved) if (valid(item)) pending.set(item.projectId, item);
    } catch (error) { /* Corrupt storage never becomes a command payload. */ }
    function changed() { storage.set(storageKey, JSON.stringify([...pending.values()])); if (!stopped) onChange(); }
    function publish(state) {
      if (stopped || !state || !/^[a-f0-9]{32}$/.test(state.id) || !Number.isSafeInteger(state.revision)) return;
      if ((revisions.get(state.id) ?? -1) > state.revision) return;
      revisions.set(state.id, state.revision); onState(state);
    }
    function observe(state) { if (state && Number.isSafeInteger(state.revision)) revisions.set(state.id, Math.max(revisions.get(state.id) ?? -1, state.revision)); }
    async function submit(project, role, kind, payload) {
      if (stopped) throw Error('页面已停止读取，请重新认证。');
      if (pending.has(project.id)) throw Error('本项目操作结果待核实；请先只读核实，不要重复提交。');
      if (pending.size >= 50) throw Error('待核实项目已达上限，请先核实已有操作。');
      const item = { projectId: project.id, role, commandId: makeId(), kind, expectedRevision: project.revision, time: now() };
      if (!valid(item)) throw Error('Agent 命令标识无效。');
      observe(project); pending.set(project.id, item); changed();
      try {
        const state = await api('/api/command', { projectId: item.projectId, commandId: item.commandId, expectedRevision: item.expectedRevision, kind, payload });
        if (state?.id !== item.projectId || !Number.isSafeInteger(state.revision) || state.revision <= item.expectedRevision) throw Error('命令响应无法核实，请查询回执。');
        publish(state);
        if (!stopped && pending.get(item.projectId) === item) { pending.delete(item.projectId); changed(); }
        return state;
      } catch (error) {
        if (!stopped && error.definitiveRejected === true && pending.get(item.projectId) === item) { pending.delete(item.projectId); changed(); }
        throw error;
      }
    }
    function reconcile(projectId) {
      if (stopped || !pending.has(projectId)) return Promise.resolve();
      if (reads.has(projectId)) return reads.get(projectId).promise;
      const item = pending.get(projectId), controller = new AbortController();
      const promise = (async () => {
        const receipt = await api('/api/receipts?scope=project&project=' + projectId + '&command=' + encodeURIComponent(item.commandId), undefined, false, { signal: controller.signal });
        if (stopped || controller.signal.aborted || pending.get(projectId) !== item || receipt.found !== true) return;
        const data = await api('/api/state', undefined, false, { signal: controller.signal });
        if (stopped || controller.signal.aborted || pending.get(projectId) !== item) return;
        const current = data.projects?.find(state => state.id === projectId);
        if (!current || !Number.isSafeInteger(current.revision) || current.revision <= item.expectedRevision) throw Error('已找到回执，但当前状态尚未核实。');
        publish(current); pending.delete(projectId); changed();
      })().finally(() => { if (reads.get(projectId)?.controller === controller) reads.delete(projectId); });
      reads.set(projectId, { promise, controller }); return promise;
    }
    return { submit, reconcile, observe, pending: id => pending.has(id) ? { ...pending.get(id) } : null,
      stop() { stopped = true; for (const read of reads.values()) read.controller.abort(); } };
  }
  function describeRoles(project) {
    const result = {};
    for (const role of Object.keys(roles)) {
      const attempt = project?.activeAttempt, same = attempt?.role === role;
      let status = '待命；无本次启动记录';
      if (project?.leaseUnknown || project?.recoveryRequired) status = '进程状态未知，禁止继续';
      else if (same) status = attempt.status === 'queued' ? '排队中，尚未确认启动' : '后台已认领，CLI 启动待核实';
      else if (project?.agentControl?.role === role && project.agentControl.resumable) status = '已停稳，等待明确恢复';
      else if (project?.lastAttempt?.role === role) status = '上一阶段已结束';
      const task = project?.tasks?.find(task => same && task.id === attempt.taskId);
      const work = { generate: '生成设计文档', split: '拆分确认设计', regenerate: '重做驳回任务', develop: '开发获批任务', repair: '按实际缺陷返修', test: '独立测试', consult: '只读咨询，不推进原工作' };
      result[role] = { status, task: same ? task ? task.id + ' · ' + task.title : work[attempt.kind] || '等待后台确认任务' : '当前未派工' };
    }
    return result;
  }
  function mount({ node, api, storage, onState }) {
    const panel = node('section', 'agent-management'); panel.id = 'agent-management'; panel.setAttribute('aria-labelledby', 'agent-title');
    const title = node('h2', '', '三个岗位，串行接力'); title.id = 'agent-title';
    const count = node('p', 'field-help'); count.id = 'agent-count';
    const selectors = node('div', 'agent-selectors'); selectors.setAttribute('role', 'group'); selectors.setAttribute('aria-label', '选择对话岗位');
    const buttons = {};
    for (const [role, label] of Object.entries(roles)) {
      const button = node('button', 'agent-selector'); button.type = 'button'; button.dataset.agentRole = role; button.setAttribute('aria-pressed', 'false');
      button.append(node('strong', '', label), node('span'), node('small')); selectors.append(button); buttons[role] = button;
      button.addEventListener('click', () => selectRole(role));
    }
    const heading = node('h3'); heading.id = 'agent-thread-title';
    const reading = node('p', 'field-help'); reading.id = 'agent-thread-status'; reading.setAttribute('role', 'status');
    const thread = node('div', 'agent-thread'); thread.id = 'agent-thread'; thread.setAttribute('aria-labelledby', heading.id);
    const pages = node('div', 'agent-actions');
    const previous = node('button', 'button small', '上一页'), next = node('button', 'button small', '下一页');
    previous.type = next.type = 'button'; previous.id = 'agent-previous'; next.id = 'agent-next'; pages.append(previous, next);
    const draftLabel = node('label', 'field-label', '给此岗位补充说明'); draftLabel.htmlFor = 'agent-draft';
    const draft = node('textarea'); draft.id = 'agent-draft'; draft.maxLength = 4000; draft.rows = 3; draft.placeholder = '只保存指导不会启动模型；当前工具完成后，由 Agent 显式收件并报告应用。';
    const intentLabel = node('label', 'agent-intent', '指令类型'); intentLabel.htmlFor = 'agent-intent';
    const intent = node('select'); intent.id = 'agent-intent';
    for (const [value, label] of [['clarification', '范围内补充说明'], ['scope_change', '改变已确认范围（必须先处理批准）']]) { const option = node('option', '', label); option.value = value; intent.append(option); }
    intentLabel.append(intent);
    const send = node('button', 'button', '保存消息（不启动模型）'); send.type = 'button'; send.id = 'agent-send';
    const commandStatus = node('p', 'field-help'); commandStatus.id = 'agent-command-status'; commandStatus.setAttribute('role', 'status');
    const reconcile = node('button', 'button small', '只读核实操作结果'); reconcile.type = 'button'; reconcile.id = 'agent-reconcile';
    const reason = node('p', 'agent-control-reason'); reason.id = 'agent-control-reason';
    const controls = node('div', 'agent-actions');
    const handoff = node('button', 'button small', '请求安全交接'), consult = node('button', 'button small', '咨询选中问题（可能计费）'), resume = node('button', 'button small', '明确恢复原工作（新一轮，可能计费）');
    handoff.type = consult.type = resume.type = 'button'; handoff.id = 'agent-handoff'; consult.id = 'agent-consult'; resume.id = 'agent-resume'; controls.append(handoff, consult, resume);
    const hint = node('p', 'field-help'); hint.id = 'agent-control-hint';
    const warning = node('p', 'field-help'); warning.id = 'agent-storage-warning';
    panel.append(title, count, selectors, heading, reading, thread, pages, draftLabel, draft, intentLabel, send, commandStatus, reconcile, reason, controls, hint, warning);
    document.getElementById('current-progress').after(panel);
    let project = null, online = false, ready = false, stopped = false, role = 'architect', generation = 0, scope = '', offset = 0, cursors = [], page = null, message = '', checking = false;
    let rowsSignature = '', rows = [], threadFailed = false;
    const drafts = new Map(), selected = new Set(), roleChoice = new Map();
    const commands = createCommands({ api, storage, onState, onChange: update });
    const labels = { queued: '已保存，等待收件', received: '已收件，尚未应用', applied: '已应用（Agent 公开回执）', needs_confirmation: '需要范围确认，不能自动应用', superseded: '已撤回 / 已替代', expired: '上下文已失效', answered: '咨询已答复，不等于原任务完成' };
    const poll = globalThis.RelaySession.createPoll({
      read: options => {
        const expected = generation, id = project.id, selectedRole = role;
        return api('/api/agents/thread?project=' + id + '&role=' + role + '&after=' + offset + '&limit=50', undefined, false, options)
          .then(value => ({ value, expected, id, selectedRole }));
      },
      success: result => {
        if (stopped || !online || result.expected !== generation || result.id !== project?.id || result.selectedRole !== role) return;
        const value = result.value;
        if (value.projectId !== project.id || value.role !== role || !Array.isArray(value.messages) || value.messages.length > 50 ||
          !Number.isSafeInteger(value.nextCursor) || value.nextCursor < offset) throw Error('线程响应与当前岗位不匹配。');
        rows = value.messages; page = value; threadFailed = false;
        const allowed = new Set(rows.filter(item => ['queued', 'received'].includes(item.status) && item.intent === 'clarification' && item.author !== 'agent').map(item => item.id));
        for (const id of selected) if (!allowed.has(id)) selected.delete(id);
        renderRows(); reading.textContent = '当前岗位历史已同步；保存、收件、应用分别记录。'; update();
      },
      failure: error => {
        threadFailed = true; selected.clear();
        reading.textContent = '线程读取失败：' + error.message + '；旧记录不是当前收件状态。';
        if (error.category === 'http' && error.status === 403) stop(); else update();
      }
    });
    function draftKey() { return project ? project.id + '/' + role : ''; }
    function rememberDraft() {
      if (!project) return;
      const key = draftKey(); drafts.delete(key); drafts.set(key, { text: draft.value, intent: intent.value });
      while (drafts.size > 30) drafts.delete(drafts.keys().next().value);
    }
    function restoreDraft() { const saved = drafts.get(draftKey()); draft.value = saved?.text || ''; intent.value = saved?.intent || 'clarification'; }
    function changeThread() {
      generation++; poll.stop(); offset = 0; cursors = []; page = null; rows = []; rowsSignature = ''; selected.clear(); thread.replaceChildren(); threadFailed = false;
      reading.textContent = online && project ? '正在读取此岗位历史…' : '未连接，当前状态无法核实。';
      if (online && project && !stopped) poll.start();
    }
    function selectRole(value) {
      if (!Object.hasOwn(roles, value) || role === value) return;
      rememberDraft(); role = value; roleChoice.delete(project?.id); roleChoice.set(project?.id, role);
      while (roleChoice.size > 50) roleChoice.delete(roleChoice.keys().next().value);
      restoreDraft(); changeThread(); update();
    }
    function renderRows() {
      const signature = JSON.stringify(rows);
      if (signature === rowsSignature) return;
      rowsSignature = signature;
      const elements = rows.map(item => {
        const row = node('article', 'agent-message'); row.dataset.messageId = item.id; row.dataset.status = item.status;
        const meta = node('div', 'agent-message-meta');
        if (['queued', 'received'].includes(item.status) && item.intent === 'clarification' && item.author !== 'agent') {
          const label = node('label', 'agent-select-message'); const box = node('input'); box.type = 'checkbox'; box.checked = selected.has(item.id); box.setAttribute('aria-label', '选入咨询或恢复：' + String(item.text).slice(0, 40));
          box.addEventListener('change', () => { if (box.checked) selected.add(item.id); else selected.delete(item.id); update(); }); label.append(box, node('span', '', '选入')); meta.append(label);
        }
        meta.append(node('strong', '', item.author === 'agent' ? roles[role] + ' · 公开答复' : '你给' + roles[role] + '的消息'), node('span', '', labels[item.status] || '状态待核实'));
        row.append(meta, node('p', 'agent-message-text', item.text));
        if (item.summary) row.append(node('p', 'agent-message-summary', item.summary));
        if (item.status === 'needs_confirmation') {
          row.append(node('p', 'field-help', '聊天不能批准范围变更。可撤回后走原设计/任务审核；只有确属范围内解释时，才重新保存为补充说明。'));
          const clarify = node('button', 'button small', '将草稿作为范围内解释'); clarify.type = 'button'; clarify.dataset.agentMutation = 'true';
          clarify.addEventListener('click', () => {
            if (!draft.value.trim()) { message = '先在下方填写范围内解释；不会自动批准范围变化。'; update(); draft.focus(); return; }
            if (confirm('确认草稿只是原批准范围内的解释？此操作不批准新设计或新任务。')) mutate('agent_resolve', { messageId: item.id, decision: 'clarification', text: draft.value.trim() });
          }); row.append(clarify);
        }
        if (['queued', 'received', 'needs_confirmation'].includes(item.status)) {
          const withdraw = node('button', 'button small', '撤回此指令'); withdraw.type = 'button'; withdraw.dataset.agentMutation = 'true';
          withdraw.addEventListener('click', () => { if (confirm('撤回此指令？不会撤销已发生的文件修改，也不批准新范围。')) mutate('agent_resolve', { messageId: item.id, decision: 'supersede', text: '用户明确撤回此指令' }); }); row.append(withdraw);
        }
        if (item.author === 'agent') {
          const quote = node('button', 'button small', '引用为原岗位指导草稿'); quote.type = 'button';
          quote.addEventListener('click', () => {
            const target = project?.agentControl?.role;
            if (!target || !Object.hasOwn(roles, target)) { message = '没有可恢复的原岗位；请先选择岗位再编写指导。'; update(); return; }
            selectRole(target); draft.value = item.text; intent.value = 'clarification'; rememberDraft(); update(); draft.focus();
          }); row.append(quote);
        }
        return row;
      });
      if (!elements.length) elements.push(node('p', 'empty-state', '此页还没有消息。补充说明只会保存，不会偷偷启动岗位。'));
      thread.replaceChildren(...elements);
    }
    function update() {
      const pending = project && commands.pending(project.id), disabled = stopped || !online || !project || !!pending;
      const descriptions = describeRoles(project), active = project?.activeAttempt, control = project?.agentControl;
      heading.textContent = roles[role] + ' · 消息与公开回执';
      for (const [key, button] of Object.entries(buttons)) {
        button.setAttribute('aria-pressed', String(key === role)); button.querySelector('span').textContent = online ? descriptions[key].status : '未连接，当前状态未知';
        button.querySelector('small').textContent = descriptions[key].task;
      }
      const recorded = (project?.executions || []).filter(item => Number.isInteger(item.pid) && item.pid > 0).length;
      count.textContent = '3 个岗位，最多 1 个执行阶段；' + (active ? active.status === 'queued' ? '当前排队 1，尚未确认 CLI 启动。' : '后台认领 1，实际 CLI 活动查看上方进度。' : '当前无活动阶段。') + ' 已保留实际进程证据 ' + recorded + ' 条。';
      if (!online) count.textContent = '3 个岗位；当前未连接，不能核实启动或停止数量。';
      draft.disabled = intent.disabled = stopped || !project;
      send.disabled = disabled || !draft.value.trim();
      commandStatus.textContent = pending ? '本项目操作结果待核实；切换岗位也不会解锁。仅查询回执和当前状态，不重发。' : message;
      reconcile.hidden = !pending; reconcile.disabled = !online || stopped || checking;
      previous.disabled = !online || stopped || !cursors.length; next.disabled = !online || stopped || !page?.hasMore || cursors.length >= 50;
      reason.textContent = control?.reason || project?.lastError || '新说明等当前工具结束后处理；保存成功不是 Agent 已收件或应用。';
      const unknown = project?.leaseUnknown || project?.recoveryRequired || project?.phase === 'stopping';
      handoff.disabled = disabled || unknown || !active || active.kind === 'consult' || control?.status === 'handoff_requested';
      const canExecute = !disabled && !unknown && ready && !active;
      const conflicts = Object.keys(roles).filter(key => (project?.agentInbox?.[key]?.needs_confirmation || 0) > 0);
      consult.disabled = !canExecute || !selected.size || threadFailed;
      resume.disabled = !canExecute || !control?.resumable || control.role !== role || threadFailed || conflicts.length > 0;
      hint.textContent = disabled && pending ? '先核实未知提交；不能用换岗、刷新或重复点击绕过。' :
        unknown ? '旧进程或恢复状态未知，禁止咨询与恢复；保留数据和锁。' :
        active ? active.kind === 'consult' ? '只读咨询进行中；答复不会完成原任务，也不会自动恢复。' : '咨询前须请求交接并等待后台确认所有执行停稳；交接请求不是已暂停。' :
        conflicts.length ? conflicts.map(key => roles[key]).join('、') + '有待确认的范围冲突；先切换对应岗位明确处理，再恢复原工作。聊天不能批准新范围。' :
        !ready ? '接入检测尚未就绪：消息可保存，咨询与恢复仍禁用。' :
        '咨询需勾选本岗位问题；恢复只带入你勾选的原岗位说明，未选旧指导会失效。恢复启动新一轮，可能计费。';
      warning.hidden = storage.available !== false;
      warning.textContent = '浏览器存储不可用：待核实标识只在此标签内存；刷新会丢失。不要刷新后重复操作，先保留页面并核实后台记录。消息草稿始终不写浏览器存储。';
      for (const button of thread.querySelectorAll('[data-agent-mutation]')) button.disabled = disabled || threadFailed;
      for (const box of thread.querySelectorAll('input[type=checkbox]')) box.disabled = disabled || threadFailed;
    }
    async function mutate(kind, payload) {
      if (stopped || !online || !project || commands.pending(project.id)) return;
      const originalId = project.id, originalRole = role, source = draft.value; message = '';
      try {
        await commands.submit(project, role, kind, payload);
        if (project?.id === originalId && role === originalRole && !stopped) {
          if (kind === 'agent_message' && draft.value === source) { draft.value = ''; rememberDraft(); }
          message = kind === 'agent_message' ? '消息已保存；等待 Agent 显式收件与应用，不代表已经执行。' : kind === 'agent_handoff' ? '交接请求已保存；不代表进程已经停稳。等待后台确认。' : kind === 'agent_consult' ? '已请求只读咨询；原工作不会自动恢复。' : kind === 'agent_resume' ? '已请求新一轮恢复；尚未确认 CLI 启动。' : '指令处理已保存；不撤销已发生的操作。';
          selected.clear(); poll.wake();
        }
      } catch (error) { if (project?.id === originalId && role === originalRole && !stopped) message = commands.pending(originalId) ? '操作结果待核实，不要重发：' + error.message : '明确拒绝，未接收本次操作：' + error.message; }
      finally { update(); }
    }
    draft.addEventListener('input', () => { rememberDraft(); update(); }); intent.addEventListener('change', rememberDraft);
    send.addEventListener('click', () => mutate('agent_message', { role, text: draft.value.trim(), intent: intent.value, targetAttemptId: project?.activeAttempt?.role === role ? project.activeAttempt.id : null }));
    handoff.addEventListener('click', () => mutate('agent_handoff', {}));
    consult.addEventListener('click', () => { if (confirm('将启动独立只读咨询，可能计费；咨询后不会自动恢复原工作。确认咨询选中问题？')) mutate('agent_consult', { role, messageIds: [...selected] }); });
    resume.addEventListener('click', () => { if (confirm('将启动新一轮原岗位执行，可能计费；不是原进程续跑。只带入勾选说明，未选旧指导会失效。确认明确恢复？')) mutate('agent_resume', { messageIds: [...selected] }); });
    reconcile.addEventListener('click', async () => {
      if (checking || !project || !online || stopped) return;
      const id = project.id; checking = true; update();
      try {
        await commands.reconcile(id);
        if (project?.id === id && !stopped) { message = commands.pending(id) ? '尚未找到可核实回执；缺少回执不等于已拒绝，仍禁止重发。' : '已找到回执并读取当前状态；未重发操作。'; poll.wake(); }
      } catch (error) { if (project?.id === id && !stopped) message = '只读核实失败：' + error.message; }
      finally { checking = false; update(); }
    });
    previous.addEventListener('click', () => { if (!cursors.length) return; generation++; poll.stop(); offset = cursors.pop(); selected.clear(); page = null; rowsSignature = ''; thread.replaceChildren(); poll.start(); update(); });
    next.addEventListener('click', () => { if (!page?.hasMore || cursors.length >= 50) return; generation++; poll.stop(); cursors.push(offset); offset = page.nextCursor; selected.clear(); page = null; rowsSignature = ''; thread.replaceChildren(); poll.start(); update(); });
    function stop() { if (stopped) return; stopped = true; generation++; poll.stop(); commands.stop(); selected.clear(); reading.textContent = '已停止读取；重新认证前不能核实新的收件或运行状态。'; update(); }
    return {
      setProject(value, connected, setupReady) {
        if (stopped) return;
        const changed = project?.id !== value?.id, nextScope = [value?.id, value?.epoch, value?.activeAttempt?.id].join('/');
        if (changed) { rememberDraft(); role = roleChoice.get(value?.id) || value?.activeAttempt?.role || value?.agentControl?.role || 'architect'; message = ''; }
        const restart = nextScope !== scope || online !== !!connected;
        project = value; online = !!connected; ready = !!setupReady; commands.observe(project);
        if (changed) restoreDraft();
        if (restart) { scope = nextScope; changeThread(); }
        update();
      }, stop, pending: id => commands.pending(id)
    };
  }
  return { createCommands, describeRoles, mount };
});
