(function (root) {
  'use strict';
  const clients = ['Claude Code', 'Codex', 'BoBot'];
  const planningPhases = ['draft', 'document', 'uploaded', 'review', 'approved', 'stopped', 'ready', 'accepted'];
  const sampleTasks = [
    { id: 'D1', title: '地点卡片与分类浏览', detail: '本地示例数据、搜索与分类；所有地点注明待体验、信息待核实。', owner: 'developer',
      acceptance: ['名称与区域可搜索，分类可切换。', '无匹配结果时显示空状态；所有地点标记待核实。'], dependsOn: [] },
    { id: 'D2', title: '收藏交互与移动端适配', detail: '收藏仅保存在本机浏览器；存储不可用时回退，不接登录或云同步。', owner: 'developer',
      acceptance: ['收藏与取消收藏可操作，存储失败回退内存。', '375px窄屏没有横向滚动。'], dependsOn: [] },
    { id: 'T1', title: '独立测试与交付检查', detail: '检查筛选、收藏、窄屏布局及页面错误；记录实际命令与结果，交给你验收。', owner: 'tester',
      acceptance: ['验证D1与D2当前版本的验收点。', '测试失败先返修复测，通过后等待人工验收。'], dependsOn: ['D1', 'D2'] }
  ];
  function createState() {
    return { phase: 'draft', revision: 1, epoch: 0, sequence: 0,
      requirement: '做一个上海遛娃小工具：用本地示例地点，支持搜索、分类和收藏，手机上也能用。所有地点标注待体验、信息待核实；不用登录，不部署到公网。',
      roles: { architect: 'Claude Code', developer: 'Codex', tester: 'BoBot' },
      generatedDocument: null, uploadedDocument: null, documentCounter: 0, generationCounter: 0,
      taskCounter: 0, uploadCounter: 0, pendingUpload: null, uploadError: '',
      tasks: [], events: [], pausedPhase: null };
  }
  function token(state) { return `${state.epoch}:${state.revision}:${state.sequence}`; }
  function validateDocumentFile(name, bytes) {
    if (typeof name !== 'string' || !/\.(md|txt)$/i.test(name)) return { ok: false, error: '请选择 .md 或 .txt 文档。' };
    if (!(bytes instanceof Uint8Array)) return { ok: false, error: '无法读取文件字节。' };
    if (bytes.byteLength > 256 * 1024) return { ok: false, error: '文件超过256KiB，请精简后重试。' };
    let text;
    try { text = new TextDecoder('utf-8', { fatal: true }).decode(bytes); }
    catch (_) { return { ok: false, error: '文件不是有效UTF-8，请以UTF-8编码保存后上传。' }; }
    if (!text.trim()) return { ok: false, error: '文档不能为空或只有空白。' };
    if (/[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f]/.test(text)) return { ok: false, error: '文档含非文本控制字符，请上传纯文本。' };
    return { ok: true, text };
  }
  function validDocument(state) {
    return !!state.uploadedDocument && state.uploadedDocument.revision === state.revision && !!state.requirement.trim();
  }
  function currentApprovals(state) {
    if (!validDocument(state) || !state.tasks.length) return false;
    const byId = new Map(state.tasks.map(item => [item.id, item]));
    if (byId.size !== state.tasks.length) return false;
    return state.tasks.every(item => {
      const approval = item.approval;
      return item.documentVersion === state.uploadedDocument.version && item.status === 'approved' &&
        !!item.title.trim() && approval && approval.version === item.version &&
        approval.documentVersion === item.documentVersion && item.dependsOn.every(id =>
          byId.has(id) && id !== item.id && approval.dependencies[id] === byId.get(id).version);
    });
  }
  function canStart(state) { return state.phase === 'approved' && !state.pendingUpload && !!currentApprovals(state); }
  function transition(state, action) {
    if (!action || action.token !== token(state)) return state;
    const next = structuredClone(state);
    const record = (message, channel = 'workflow', from = '工作台', to = '你') => {
      next.events.push({ number: next.events.length + 1, simulated: true, channel, from, to, message });
    };
    const history = (item, type, message) => item.history.push({ type, version: item.version, documentVersion: item.documentVersion, message });
    const invalidateDownstream = id => {
      const affected = new Set([id]);
      let changed = true;
      while (changed) {
        changed = false;
        for (const item of next.tasks) {
          if (!affected.has(item.id) && item.dependsOn.some(dependency => affected.has(dependency))) {
            affected.add(item.id);
            changed = true;
          }
        }
      }
      for (const item of next.tasks) {
        if (item.id !== id && affected.has(item.id) && ['approved', 'needs-review'].includes(item.status)) {
          item.status = 'needs-review';
          item.approval = null;
          item.reviewReason = `上游任务${id}已驳回或升版，请重新检查依赖与验收范围。`;
          history(item, 'needs-review', item.reviewReason);
        }
      }
    };
    const clearDocuments = () => {
      next.generatedDocument = null;
      next.uploadedDocument = null;
      next.tasks = [];
      next.pendingUpload = null;
      next.uploadError = '';
      next.pausedPhase = null;
    };
    switch (action.type) {
      case 'edit': {
        let changed = false;
        if (action.field === 'requirement' && typeof action.value === 'string' && action.value.length <= 4000) {
          changed = action.value !== state.requirement;
          next.requirement = action.value;
        } else if (action.field === 'role' && Object.prototype.hasOwnProperty.call(state.roles, action.role) && clients.includes(action.value)) {
          changed = next.roles[action.role] !== action.value;
          next.roles[action.role] = action.value;
        }
        if (!changed) return state;
        next.revision++;
        clearDocuments();
        next.phase = 'draft';
        if (state.uploadedDocument || state.generatedDocument || state.pendingUpload) record('需求或角色已变更，旧文档、任务批准及演示结果失效。请重新生成或上传设计。');
        break;
      }
      case 'generate-document':
        if (!planningPhases.includes(state.phase) || !state.requirement.trim()) return state;
        clearDocuments();
        next.generationCounter++;
        next.generatedDocument = { version: next.generationCounter, revision: state.revision,
          text: `# 小步出门 · 设计文档（预制示例）\n\n> 静态原型模拟生成，未调用AI。请人工调整后上传；不是模型根据任意需求完成的设计。\n\n## 项目需求\n${state.requirement}\n\n## 目标与边界\n本地上海示例地点，支持搜索、分类与收藏，适配手机。\n所有地点待体验、信息待核实；不接登录、云同步或公网发布。\n\n## 页面与数据\n静态HTML / CSS / JavaScript，使用本地地点数据。\n收藏保存浏览器，存储失败回退内存。\n\n## 分工与交付\n架构师：${state.roles.architect}；开发者：${state.roles.developer}；测试员：${state.roles.tester}。\n共享目录串行交接，真实版保留reason-network。\n人工修改本设计并上传，再拆任务；每项审核通过后才能开始开发。\n\n## 验收点\n- 搜索、分类和空结果提示可用。\n- 收藏可增删，存储不可用不阻断使用。\n- 375px无横向滚动；测试通过后仍需人工验收。\n` };
        next.phase = 'document';
        record(`载入设计示例G${next.generationCounter}，请下载、人工修改并上传；没有生成任务或调用模型。`);
        record('DESIGN_DRAFT：示例设计等待人工修改与上传（模拟，未发送）。', 'reason', 'demo-arch-01', 'demo-control');
        break;
      case 'upload-begin':
        if (!planningPhases.includes(state.phase) || !state.requirement.trim()) return state;
        next.uploadCounter++;
        next.pendingUpload = { epoch: state.epoch, revision: state.revision, request: next.uploadCounter };
        next.uploadError = '';
        break;
      case 'upload-success':
      case 'upload-failure': {
        const ticket = action.ticket, pending = state.pendingUpload;
        if (!ticket || !pending || ticket.epoch !== pending.epoch || ticket.revision !== pending.revision || ticket.request !== pending.request) return state;
        next.pendingUpload = null;
        const validated = action.type === 'upload-success' ? validateDocumentFile(action.name, action.bytes) :
          { ok: false, error: '文件读取失败，请重新选择。' };
        if (!validated.ok) {
          next.uploadError = validated.error;
          break;
        }
        next.documentCounter++;
        next.uploadedDocument = { version: next.documentCounter, revision: state.revision, name: action.name, text: validated.text };
        next.tasks = [];
        next.uploadError = '';
        next.phase = 'uploaded';
        next.pausedPhase = null;
        record(`已在本页读取“${action.name}”为上传文档U${next.documentCounter}；旧任务和批准失效。文件未传到服务器。`);
        break;
      }
      case 'split-tasks':
        if (!planningPhases.includes(state.phase) || state.pendingUpload || !validDocument(state) || (state.tasks.length && action.confirmed !== true)) return state;
        next.taskCounter++;
        next.tasks = sampleTasks.map(item => ({ ...structuredClone(item), version: next.taskCounter,
          documentVersion: state.uploadedDocument.version, status: 'pending', rejectionReason: '', reviewReason: '', approval: null,
          history: [{ type: 'created', version: next.taskCounter, documentVersion: state.uploadedDocument.version, message: '载入预制任务，等待人工逐项审核。' }] }));
        next.phase = 'review';
        record(`模拟拆分：预制遛娃任务关联“${state.uploadedDocument.name}”U${state.uploadedDocument.version}；未解析正文或调用AI。逐项通过前不能开发。`);
        record(`TASK_DRAFT U${state.uploadedDocument.version}：3项等待逐项审核（模拟，未发送）。`, 'reason', 'demo-arch-01', 'demo-control');
        break;
      case 'approve-task':
      case 'reject-task':
      case 'regenerate-task': {
        if (!['review', 'approved'].includes(state.phase) || state.pendingUpload || !validDocument(state)) return state;
        const item = next.tasks.find(item => item.id === action.task);
        if (!item || item.version !== action.version || item.documentVersion !== action.documentVersion || item.documentVersion !== state.uploadedDocument.version) return state;
        if (action.type === 'approve-task') {
          if (!['pending', 'needs-review'].includes(item.status) || !item.title.trim() || item.dependsOn.some(id => {
            const dependency = next.tasks.find(task => task.id === id);
            return !dependency || ['rejected', 'needs-review'].includes(dependency.status);
          })) return state;
          item.approval = { version: item.version, documentVersion: item.documentVersion,
            dependencies: Object.fromEntries(item.dependsOn.map(id => [id, next.tasks.find(task => task.id === id).version])) };
          item.status = 'approved';
          item.reviewReason = '';
          history(item, 'approved', '人工通过当前文档与任务版本。');
          record(`你已通过${item.id} v${item.version} / U${item.documentVersion}。`);
        } else if (action.type === 'reject-task') {
          if (typeof action.reason !== 'string' || !action.reason.trim() || action.reason.length > 2000 || item.status === 'rejected') return state;
          item.status = 'rejected';
          item.rejectionReason = action.reason.trim();
          item.approval = null;
          history(item, 'rejected', item.rejectionReason);
          invalidateDownstream(item.id);
          record(`你已驳回${item.id} v${item.version}：${item.rejectionReason}。仅此项可局部重做。`);
        } else {
          if (item.status !== 'rejected') return state;
          const reason = item.rejectionReason;
          next.taskCounter++;
          item.version = next.taskCounter;
          item.detail = `${sampleTasks.find(task => task.id === item.id).detail}\n模拟修订关注点：${reason}（仅记录反馈，未调用AI解决）。`;
          item.acceptance = [...sampleTasks.find(task => task.id === item.id).acceptance, `重新审核：${reason}`];
          item.status = 'pending';
          item.approval = null;
          item.reviewReason = '局部重做的新版本，必须再次人工审核。';
          history(item, 'regenerated', `依据反馈模拟补充，待重审：${reason}`);
          invalidateDownstream(item.id);
          record(`模拟重做${item.id} → v${item.version}，等待再次审核；无关任务不变，下游批准显式撤销。`);
          record(`TASK_REVISED ${item.id} v${item.version} / U${item.documentVersion}（模拟，未发送）。`, 'reason', 'demo-arch-01', 'demo-control');
        }
        next.phase = currentApprovals(next) ? 'approved' : 'review';
        break;
      }
      case 'start':
        if (!canStart(state)) return state;
        next.phase = 'developing';
        record('全部任务当前版本已通过；你手动开始开发阶段演示，仅模拟唯一写入者交接。');
        record(`ASSIGN U${state.uploadedDocument.version} → 开发者（模拟派工，未发送）`, 'reason', 'demo-control', 'demo-dev-02');
        break;
      case 'develop-complete':
        if (state.phase !== 'developing' || !currentApprovals(state)) return state;
        next.phase = 'testing';
        record('模拟开发结束，交接独立测试；没有真实文件变更。');
        record('CHANGES_READY → 测试员（模拟，未发送）', 'reason', 'demo-dev-02', 'demo-test-03');
        break;
      case 'test-fail':
        if (state.phase !== 'testing' || !currentApprovals(state)) return state;
        next.phase = 'failed';
        record('模拟测试失败：窄屏收藏状态未更新。禁止验收，需要返修。');
        record('TEST_FAILED → 开发者（模拟缺陷，非真实发现）', 'reason', 'demo-test-03', 'demo-dev-02');
        break;
      case 'fix':
        if (state.phase !== 'failed' || !currentApprovals(state)) return state;
        next.phase = 'fixing';
        record('将原批准范围内缺陷模拟交回开发者，不扩展需求。');
        record('FIX_REQUEST → 开发者（模拟，未发送）', 'reason', 'demo-control', 'demo-dev-02');
        break;
      case 'fix-complete':
        if (state.phase !== 'fixing' || !currentApprovals(state)) return state;
        next.phase = 'testing';
        record('模拟修复完成，必须重新测试。');
        record('FIX_READY → 测试员（模拟，未发送）', 'reason', 'demo-dev-02', 'demo-test-03');
        break;
      case 'test-pass':
        if (state.phase !== 'testing' || !currentApprovals(state)) return state;
        next.phase = 'ready';
        record('模拟测试通过，等待你验收；没有运行BoBot或真实测试命令。');
        record('TEST_REPORT → 主控（模拟结果，未发送）', 'reason', 'demo-test-03', 'demo-control');
        break;
      case 'accept':
        if (state.phase !== 'ready' || state.pendingUpload || !currentApprovals(state)) return state;
        next.phase = 'accepted';
        record('你已完成原型的模拟验收，不代表真实项目已经交付。');
        break;
      case 'pause':
        if (!['developing', 'testing', 'fixing'].includes(state.phase)) return state;
        next.pausedPhase = state.phase;
        next.phase = 'paused';
        record('前端演示暂停，没有后台进程需要停止。');
        break;
      case 'resume':
        if (state.phase !== 'paused' || !currentApprovals(state)) return state;
        next.phase = state.pausedPhase;
        next.pausedPhase = null;
        record('继续前端演示。');
        break;
      case 'stop':
        if (['draft', 'stopped', 'accepted'].includes(state.phase) && !state.pendingUpload) return state;
        next.phase = 'stopped';
        next.pendingUpload = null;
        next.pausedPhase = null;
        for (const item of next.tasks) {
          if (item.status === 'approved') {
            item.status = 'needs-review';
            item.reviewReason = '演示已终止，需要重新拆分与审核。';
          }
          item.approval = null;
        }
        record('演示终止，批准撤销；重新上传或重新拆分审核后才可运行。未停止任何真实CLI。');
        break;
      case 'reset': {
        const fresh = createState();
        fresh.epoch = state.epoch + 1;
        return fresh;
      }
      default: return state;
    }
    next.sequence++;
    return next;
  }
  const api = { createState, transition, token, canStart, validateDocumentFile };
  if (typeof module !== 'undefined' && module.exports) module.exports = api;
  else root.WorkbenchModel = api;
})(typeof globalThis !== 'undefined' ? globalThis : this);
