(function () {
  'use strict';
  window.RelayOutput = { mount, describeProgress };
  const roles = { architect: '架构师', developer: '开发者', tester: '测试员' };
  const stages = { preparing: '准备独立进程', cli_running: 'CLI 正在运行', cli_exited: 'CLI 已退出，宿主仍需校验', host_validation: '宿主校验交付证据', trusted_rerun: '可信检查器按冻结规格复跑' };
  const tools = { receive_task: '从 reason 接收任务', send_ack: '向 reason 发送显式 ACK', send_result: '向 reason 发送 RESULT（不是验收通过）', read_project: '读取项目文件', write_project: '写入岗位允许的项目文件', run_check: '执行受控检查器' };
  function describeProgress(project, telemetry = null) {
    const phases = {
      draft: ['准备需求', '填写并保存需求，再生成设计。', '#requirement'],
      designing: ['架构师生成设计中', '等待后台生成设计，不要重复提交。'],
      document: ['设计已生成，等待人工修改上传', '下载并检查设计，修改后上传确认版本。', '#document-title'],
      uploaded: ['已上传确认设计', '按上传版本拆分任务，再逐项审核。', '#split-tasks'],
      splitting: ['架构师按上传版本拆分中', '等待后台拆分任务，不要重复提交。'],
      review: ['等待逐项审核', '逐项审核任务；全部通过也不会自动开工。', '#tasks'],
      regenerating: ['仅重做驳回任务中', '等待驳回项重做，再审核当前版本。'],
      approved: ['全部通过，等待手动开工', '核对任务后，手动开始开发。', '#start'],
      developing: ['开发者执行获批任务中', '查看执行记录，等待独立测试与可信复跑。'],
      testing: ['独立测试与可信复跑中', '等待独立测试和可信复跑；日志结束不代表通过。'],
      failed: ['实际测试失败，等待返修', '查看实际缺陷，再明确决定是否返修。'],
      fixing: ['开发者按实际缺陷返修中', '等待返修和重新测试，不要重复派工。'],
      ready: ['实际测试通过，待人工验收', '查看产物和证据，再决定是否人工验收。', '#delivery'],
      accepted: ['人工验收已记录', '查看已验收产物与记录。', '#delivery'],
      paused: ['当前阶段已结束，调度已暂停', '查看暂停状态，再决定继续或终止。'],
      stopping: ['正在终止并确认进程树', '等待后台确认进程树停止，不要清锁。'],
      stopped: ['已终止，批准已撤销', '查看终止记录；重新工作需要重新批准。'],
      blocked: ['执行阻塞，禁止自动续跑', '查看阻塞原因；不要自动重试或删除锁。']
    };
    const [title, next, target = '#runtime'] = project ? phases[project.phase] || ['后台状态未知', '核实后台状态，不自动重试。'] : ['尚未选择项目', '新建或选择当前工作台的项目。', '#my-projects'];
    const result = { title, next, target,
      detail: project?.lastError || (project?.pauseRequested ? '已请求暂停；当前阶段结束后不再派工。' : '后台状态，不以日志结束或请求成功代替完成。'),
      agent: '暂无运行中的 Agent。', task: '尚未拆分开发任务。',
      activity: '尚无本次 Agent 的可验证活动。', timing: '暂无可验证的 CLI 计时。' };
    if (!project) return result;
    const tasks = project.tasks || [], developers = new Set(tasks.filter(task => task.owner === 'developer').map(task => task.id));
    const done = new Set((project.run?.done || []).filter(id => developers.has(id)));
    if (developers.size) result.task = `开发任务已完成 ${done.size}，共 ${developers.size} 项。`;
    const attempt = project.activeAttempt;
    if (project.leaseUnknown || project.recoveryRequired) {
      result.title = '进程状态待核实，禁止继续执行'; result.next = '保留数据和锁，先核实旧进程状态。';
      result.agent = 'Agent 进程状态未知，不能确认正在运行或已经停止。';
      return result;
    }
    if (!attempt) return result;
    const role = roles[attempt.role] || 'Agent';
    const task = tasks.find(task => task.id === attempt.taskId);
    const work = { generate: '根据需求生成设计文档', split: '按确认设计拆分任务', regenerate: '重做驳回项', repair: '按实际缺陷返修', test: '独立验收与可信复跑' };
    result.task = (task ? `${task.id} · ${task.title}` : work[attempt.kind] || '等待后台确认当前任务') + (developers.size ? `；开发任务已完成 ${done.size}，共 ${developers.size} 项。` : '。');
    result.agent = `${role}：后台已认领，尚无可验证的执行细阶段。`;
    if (attempt.status === 'queued') {
      result.title = '后台已接收，等待执行'; result.next = '等待受控执行器，不要重复提交。';
      result.agent = `${role}：排队中，尚未确认启动。`;
      return result;
    }
    const scope = telemetry?.scope, authority = telemetry?.authority, meta = telemetry?.metadata;
    const bound = attempt.status === 'running' && telemetry?.available === true && telemetry.sealed !== true &&
      attempt.projectId === project.id && (attempt.runId || null) === (project.run?.id || null) &&
      scope?.projectId === project.id && scope.epoch === project.epoch && attempt.epoch === project.epoch &&
      scope.attemptId === attempt.id && scope.runId === attempt.runId && scope.inputHash === attempt.inputHash &&
      authority?.epoch === project.epoch && authority.activeAttemptId === attempt.id &&
      !authority.leaseUnknown && !authority.recoveryRequired &&
      Number.isSafeInteger(authority.revision) && authority.revision >= project.revision &&
      meta?.status === 'running' && meta.role === attempt.role && meta.kind === attempt.kind;
    if (!bound) return result;
    result.agent = `${role}：${stages[meta.stage] || '等待可验证的执行细阶段'}。`;
    const now = telemetry.serverNow;
    const valid = value => Number.isFinite(value) && value > 0 && Number.isFinite(now) && value <= now;
    const seconds = value => Math.floor(value / 1000) + ' 秒';
    if (valid(meta.startedAt)) {
      const ended = valid(meta.cliEndedAt) && meta.cliEndedAt >= meta.startedAt;
      if (meta.cliEndedAt == null || ended) result.timing = `CLI ${ended ? '实际运行' : '已运行'} ${seconds((ended ? meta.cliEndedAt : now) - meta.startedAt)}${ended ? '；已退出，校验与复跑另计。' : '；不是预计完成时间。'}`;
    }
    const activity = telemetry.activity;
    if (activity?.kind === 'tool' && tools[activity.tool] && valid(activity.at)) {
      const suffix = activity.phase === 'started' ? '开始' : activity.phase === 'finished' ? (activity.outcome === 'ok' ? '已返回' : activity.outcome === 'error' ? '失败，查看后台状态' : '') : '';
      if (suffix) result.activity = tools[activity.tool] + ' · ' + suffix;
    } else if (activity?.kind === 'text' && valid(activity.at)) result.activity = '已收到公开输出，可展开执行记录查看。';
    else if (activity?.kind === 'stage' && stages[activity.stage] && valid(activity.at)) result.activity = stages[activity.stage];
    if (valid(meta.lastEventAt) && valid(meta.startedAt) && meta.lastEventAt >= meta.startedAt && now - meta.lastEventAt >= 30000) result.activity = `已 ${seconds(now - meta.lastEventAt)} 无新可展示活动；不能据此判断执行结果。`;
    return result;
  }
  function mount({ node, api, observed = () => {} }) {
    const $ = id => document.getElementById(id);
    const button = node('button', '', 'Claude 实时输出');
    button.id = 'tab-claude'; button.type = 'button'; button.setAttribute('role', 'tab');
    button.setAttribute('aria-selected', 'false'); button.setAttribute('aria-controls', 'claude-panel'); button.tabIndex = -1;
    document.querySelector('.log-tabs').append(button);
    const panel = node('section', 'claude-panel'); panel.id = 'claude-panel'; panel.hidden = true;
    panel.setAttribute('role', 'tabpanel'); panel.setAttribute('aria-labelledby', button.id);
    const status = node('p', 'field-help', '尚无执行记录'); status.id = 'claude-status'; status.setAttribute('role', 'status');
    const timing = node('p', 'field-help'); timing.id = 'claude-timing';
    const follow = node('button', 'button small', '暂停跟随'); follow.id = 'claude-follow'; follow.type = 'button'; follow.setAttribute('aria-pressed', 'true');
    const output = node('div', 'claude-output'); output.id = 'claude-output'; output.tabIndex = 0; output.setAttribute('aria-label', 'Claude 公开文本与宿主工具活动');
    panel.append(status, timing, follow, output); $('event-log').after(panel);
    const stages = { preparing: '准备独立进程', cli_running: 'CLI 正在运行', cli_exited: 'CLI 已退出，宿主仍需校验', host_validation: '宿主校验交付证据', trusted_rerun: '可信检查器按冻结规格复跑' };
    const tools = { receive_task: '从 reason 接收任务', send_ack: '向 reason 发送显式 ACK', send_result: '向 reason 发送 RESULT（不是验收通过）', read_project: '读取项目文件', write_project: '写入岗位允许的项目文件', run_check: '执行受控检查器' };
    const diagnostics = { stream_gap: '输出观察发生缺口，已停止后续文本续接；不影响权威结果检查。', observer_failed: '输出观察失败，真实任务状态仍由后台确认。', malformed_record: '部分输出格式无法识别，未展示原始内容。', oversized_record: '单条输出超限，未展示原始内容。', unknown_schema: '出现未支持的输出格式，未展示原始内容。', snapshot_mismatch: '完成快照与增量不一致，未重复展示。' };
    let key = '', state = null, connected = false, generation = 0, controller = null, timer = null;
    let scope = null, cursor = 0, following = true, bytes = 0, last = null, activity = null, failures = 0, authExpired = false;
    function observe(page) {
      const pick = (value, keys) => Object.fromEntries(keys.filter(key => value && Object.hasOwn(value, key)).map(key => [key, value[key]]));
      observed({ available: page.available, sealed: page.sealed, serverNow: page.serverNow,
        scope: pick(page.scope, ['projectId', 'attemptId', 'epoch', 'runId', 'inputHash']),
        authority: pick(page.authority, ['epoch', 'revision', 'activeAttemptId', 'leaseUnknown', 'recoveryRequired']),
        metadata: pick(page.metadata, ['role', 'kind', 'status', 'stage', 'queuedAt', 'startedAt', 'cliEndedAt', 'lastEventAt']),
        activity });
    }
    function followingState(value) { following = value; follow.setAttribute('aria-pressed', String(value)); follow.textContent = value ? '暂停跟随' : '跟随最新'; }
    follow.addEventListener('click', () => { followingState(!following); if (following) output.scrollTop = output.scrollHeight; });
    output.addEventListener('wheel', () => followingState(false), { passive: true });
    output.addEventListener('touchmove', () => followingState(false), { passive: true });
    output.addEventListener('keydown', event => { if (['ArrowUp', 'PageUp', 'Home'].includes(event.key)) followingState(false); });
    output.addEventListener('scroll', () => { if (output.scrollHeight - output.scrollTop - output.clientHeight > 24) followingState(false); });
    function append(event) {
      let text = '';
      if (event.kind === 'text') text = event.text;
      else if (event.kind === 'tool' && tools[event.tool]) text = tools[event.tool] + (event.phase === 'started' ? ' · 开始' : event.outcome === 'ok' ? ' · 已返回' : ' · 失败');
      else if (event.kind === 'stage') text = stages[event.stage] || '';
      else if (event.kind === 'diagnostic') text = diagnostics[event.code] || '遥测不可用';
      if (!text) return;
      const row = node('div', 'claude-event ' + (event.kind === 'text' ? 'public-text' : 'host-activity'));
      row.dataset.seq = String(event.seq); row.textContent = text;
      const size = new TextEncoder().encode(text).length; row.dataset.bytes = String(size); bytes += size; output.append(row);
      while (output.children.length > 512 || bytes > 256 * 1024) { bytes -= Number(output.firstChild.dataset.bytes); output.firstChild.remove(); }
      if (following) output.scrollTop = output.scrollHeight;
    }
    function describe(page) {
      const meta = page.metadata || {};
      const now = page.serverNow;
      const seconds = ms => Math.max(0, Math.floor(ms / 1000)) + ' 秒';
      if (!page.available) {
        status.textContent = state?.activeAttempt?.status === 'queued' ? '任务排队中，尚未启动 CLI。' : state?.activeAttempt ? '此任务按旧方式启动，实时文本不可用；不能通过刷新转换。' : state?.lastAttempt ? '此任务按旧方式启动或尾部已轮换，实时文本不可用。' : '尚无执行记录；手动开始模型任务后显示真实公开输出。';
        timing.textContent = state?.activeAttempt ? '排队／准备已耗时 ' + seconds(now - state.activeAttempt.createdAt * 1000) + '；CLI 尚无可验证计时。' : '';
        return;
      }
      const role = { architect: '架构师', developer: '开发者', tester: '测试员' }[meta.role] || '岗位';
      const terminal = page.sealed === true || ['complete', 'failed', 'cancelled', 'interrupted'].includes(meta.status);
      const running = !terminal && meta.status === 'running';
      status.textContent = `${role} · ${scope.attemptId.slice(0, 12)} · ${terminal ? '本次输出已封存；权威阶段：' + (page.authority?.phase || '未知') : running ? stages[meta.stage] || '准备中' : '执行状态未知；等待权威状态'}。公开文本不是完成或测试通过凭证。`;
      const parts = ['独立本机 Claude 进程 / 仅 relay MCP / 串行执行'];
      if (meta.startedAt) {
        parts.push(`PID ${meta.pid} · 排队／准备 ${seconds(meta.startedAt - meta.queuedAt)} · CLI 已运行 ${seconds((meta.cliEndedAt || now) - meta.startedAt)}`);
        if (meta.timeoutAt && !meta.cliEndedAt && running) parts.push('距 CLI 实际超时上限 ' + seconds(meta.timeoutAt - now));
      } else parts.push('排队／准备 ' + seconds(now - meta.queuedAt) + '；尚无实际 CLI PID');
      parts.push('超时上限不是整体预计完成时间；CLI 结束后仍可能校验／复跑。');
      if (!output.querySelector('.public-text')) parts.push('暂无公开文本；模型可能只调用工具，工具参数和文件正文不会在这里公开。');
      const quiet = meta.lastEventAt || meta.startedAt || meta.queuedAt;
      if (running && now - quiet >= 30000) parts.push('已 ' + seconds(now - quiet) + ' 无新可展示事件；这不代表失败或正在思考。');
      if (page.gap) parts.push('早期日志已轮换，当前仅展示保留尾部。');
      timing.textContent = parts.join(' · ');
    }
    async function poll(expected) {
      if (expected !== generation || !connected || !state || controller || authExpired) return;
      timer = null;
      const request = new AbortController(); controller = request;
      const project = state.id;
      let query = '/api/telemetry?project=' + project;
      if (scope) query += `&attempt=${scope.attemptId}&epoch=${scope.epoch}&cursor=${cursor}`;
      try {
        const page = await api(query, undefined, false, { signal: request.signal });
        if (generation !== expected || state?.id !== project) return;
        if (scope && JSON.stringify(scope) !== JSON.stringify(page.scope)) throw Error('日志版本变化，请重新选择项目');
        if (!scope && page.scope) scope = page.scope;
        for (const event of page.events || []) if (Number.isSafeInteger(event.seq) && event.seq > cursor) {
          append(event); cursor = event.seq;
          if (event.kind === 'text') activity = { kind: 'text', at: event.at };
          else if (event.kind === 'tool' && tools[event.tool]) activity = { kind: 'tool', tool: event.tool, phase: event.phase, outcome: event.outcome, at: event.at };
          else if (event.kind === 'stage' && stages[event.stage]) activity = { kind: 'stage', stage: event.stage, at: event.at };
        }
        cursor = Math.max(cursor, page.nextCursor || 0); last = page; failures = 0; describe(page); observe(page);
      } catch (error) {
        if (generation === expected && error.name !== 'AbortError') {
          activity = null; observed(null);
          if (error.category === 'http' && error.status === 403) {
            authExpired = true;
            status.textContent = '页面会话已失效，请通过原启动器重新认证；日志读取已停止，不会重放模型操作。';
          } else {
            failures = Math.min(failures + 1, 4);
            status.textContent = '实时输出连接中断／不可用：' + error.message + '；仅退避重读日志，不自动重试模型。';
          }
        }
      } finally {
        if (controller === request) controller = null;
        if (generation === expected && connected && !authExpired) {
          const delay = failures ? Math.min(15000, 1200 * 2 ** failures) : last?.hasMore ? 50 : 1200;
          timer = setTimeout(() => poll(expected), delay);
        }
      }
    }
    function setProject(next, online) {
      if (authExpired) return;
      state = next; connected = online;
      const nextKey = next ? [next.id, next.epoch, next.activeAttempt?.id || next.lastAttempt?.id || ''].join('/') : '';
      if (key !== nextKey) {
        key = nextKey; generation++; clearTimeout(timer); timer = null; if (controller) controller.abort(); controller = null;
        scope = null; cursor = 0; last = null; activity = null; observed(null); bytes = 0; output.replaceChildren(); timing.textContent = ''; status.textContent = '读取真实执行记录…'; followingState(true);
        if (connected) poll(generation);
      } else if (!connected) {
        generation++; clearTimeout(timer); timer = null; if (controller) controller.abort(); controller = null;
        activity = null; observed(null);
        status.textContent = '后台连接断开；最后显示的日志不是当前进程存活证明。';
      } else if (!controller && !timer) poll(generation);
    }
    return { setProject, visible(value) { panel.hidden = !value; }, stop() { connected = false; generation++; clearTimeout(timer); timer = null; if (controller) controller.abort(); activity = null; observed(null); } };
  }
})();
