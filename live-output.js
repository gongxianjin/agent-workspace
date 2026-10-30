(function () {
  'use strict';
  window.RelayOutput = { mount };
  function mount({ node, api }) {
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
    let scope = null, cursor = 0, following = true, bytes = 0, last = null;
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
      if (expected !== generation || !connected || !state || controller) return;
      const request = new AbortController(); controller = request;
      const project = state.id;
      let query = '/api/telemetry?project=' + project;
      if (scope) query += `&attempt=${scope.attemptId}&epoch=${scope.epoch}&cursor=${cursor}`;
      try {
        const page = await api(query, undefined, false, { signal: request.signal });
        if (generation !== expected || state?.id !== project) return;
        if (scope && JSON.stringify(scope) !== JSON.stringify(page.scope)) throw Error('日志版本变化，请重新选择项目');
        if (!scope && page.scope) scope = page.scope;
        for (const event of page.events || []) if (Number.isSafeInteger(event.seq) && event.seq > cursor) { append(event); cursor = event.seq; }
        cursor = Math.max(cursor, page.nextCursor || 0); last = page; describe(page);
      } catch (error) {
        if (generation === expected && error.name !== 'AbortError') status.textContent = '实时输出连接中断／不可用：' + error.message + '；不自动重试模型。';
      } finally {
        if (controller === request) controller = null;
        if (generation === expected && connected) timer = setTimeout(() => poll(expected), last?.hasMore ? 50 : 1200);
      }
    }
    function setProject(next, online) {
      state = next; connected = online;
      const nextKey = next ? [next.id, next.epoch, next.activeAttempt?.id || next.lastAttempt?.id || ''].join('/') : '';
      if (key !== nextKey) {
        key = nextKey; generation++; clearTimeout(timer); timer = null; if (controller) controller.abort(); controller = null;
        scope = null; cursor = 0; last = null; bytes = 0; output.replaceChildren(); timing.textContent = ''; status.textContent = '读取真实执行记录…'; followingState(true);
        if (connected) poll(generation);
      } else if (!connected) {
        generation++; clearTimeout(timer); timer = null; if (controller) controller.abort(); controller = null;
        status.textContent = '后台连接断开；最后显示的日志不是当前进程存活证明。';
      } else if (!controller && !timer) poll(generation);
    }
    return { setProject, visible(value) { panel.hidden = !value; }, stop() { connected = false; generation++; clearTimeout(timer); timer = null; if (controller) controller.abort(); } };
  }
})();
