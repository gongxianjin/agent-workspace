(function () {
  'use strict';
  window.RelaySetup = { mount };
  function mount({ node, api, anchor, changed, shutdown }) {
    const panel = node('section', 'panel setup-panel'); panel.id = 'setup-panel';
    const title = node('h2', '', '首次接入 · 使用这台电脑自己的 Claude Code');
    const summary = node('p', 'field-help', '尚未检测；模型连接、网络与额度均未验证。'); summary.id = 'setup-status'; summary.setAttribute('role', 'status');
    const detail = node('p', 'field-help'); detail.id = 'setup-detail';
    const steps = node('ol', 'setup-steps');
    const install = node('a', '', '1. 打开官方 Windows 安装说明'); install.id = 'setup-install';
    install.href = 'https://code.claude.com/docs/en/setup'; install.target = '_blank'; install.rel = 'noopener noreferrer';
    const installHelp = node('p', '', '按官方说明安装原生 Claude Code 和 Git for Windows。工作台不会自动安装、更新或修改全局配置。');
    const first = node('li'); first.append(install, installHelp);
    const second = node('li', '', '2. 首次准备：在自己的终端运行 claude auth login，按提示使用自己的账号／订阅；已有提供商配置请遵循其接入说明。安装或登录后先重新检测，不要为此中断活动任务；日常双击入口即可。');
    const third = node('li', '', '3. 点击“重新检测”。执行固定 version / help / auth status 命令和工作台本地依赖、浏览器自检，不调用模型；账号、网络和模型费用由使用者自己承担。');
    steps.append(first, second, third);
    const browserStatus = node('p', 'field-help'); browserStatus.id = 'setup-browser-status'; browserStatus.setAttribute('role', 'status'); browserStatus.hidden = true;
    const browserNote = node('p', 'field-help', '测试浏览器首次需经你确认，从 Playwright 官方 CDN 下载固定版本（约173 MiB），仅存放在工作台文件夹内。请先阅读 '); browserNote.hidden = true;
    const browserTerms = node('a', '', '浏览器适用条款'); browserTerms.id = 'setup-browser-terms'; browserTerms.href = 'https://www.google.com/chrome/terms/'; browserTerms.target = '_blank'; browserTerms.rel = 'noopener noreferrer';
    browserNote.append(browserTerms, node('span', '', '。不会安装到系统、不会调用模型；失败不会自动重试。'));
    const distribution = node('details', 'setup-distribution'); distribution.id = 'setup-distribution'; distribution.hidden = true;
    const distributionTitle = node('summary', '', 'Windows 随包组件使用／再分发条件（首次请阅读）');
    const distributionText = node('pre', 'setup-distribution-text'); distributionText.id = 'setup-distribution-text';
    const distributionStatus = node('p', 'field-help'); distributionStatus.id = 'setup-distribution-status'; distributionStatus.setAttribute('role', 'status');
    distribution.append(distributionTitle, distributionText, distributionStatus);
    const actions = node('div', 'setup-actions');
    const acceptDistribution = node('button', 'button small', '我已阅读并同意当前发行条款'); acceptDistribution.id = 'setup-distribution-accept'; acceptDistribution.hidden = true;
    const prepareBrowser = node('button', 'button small', '确认并准备测试浏览器'); prepareBrowser.id = 'setup-browser-prepare'; prepareBrowser.hidden = true;
    const recheck = node('button', 'button small', '重新检测（不调用模型）'); recheck.id = 'setup-recheck';
    const confirmExisting = node('button', 'button small', '使用已有本机配置继续（接入未验证）'); confirmExisting.id = 'setup-confirm-existing'; confirmExisting.hidden = true;
    const exit = node('button', 'button text-danger', '退出工作台'); exit.id = 'shutdown-workbench';
    const note = node('p', 'field-help', '关闭浏览器标签不会停止后台。重复双击只重新打开；移动整个文件夹前，请在所有任务停稳后点击“退出工作台”。');
    const instructions = node('details', 'setup-instructions'); instructions.id = 'setup-instructions';
    instructions.append(node('summary', '', '安装、登录与退出说明'), steps, note);
    actions.append(acceptDistribution, prepareBrowser, recheck, confirmExisting, exit);
    panel.append(title, summary, detail, distribution, browserStatus, browserNote, actions, instructions); anchor.after(panel);
    let state = null, busy = false, generation = 0, expired = false;
    const activeBrowser = () => ['downloading', 'verifying', 'extracting', 'checking'].includes(state?.browser?.status);
    const pendingTerms = () => !!state?.distributionTerms?.required && !state.distributionTerms.accepted;
    const linuxBlocked = () => state?.platform?.startsWith('linux-') && state?.blockers?.includes('execution');
    function executionHint() {
      if (linuxBlocked()) return 'Linux执行器、原生依赖及浏览器沙箱尚未验证；当前仅控制台可用，模型操作保持禁用。';
      if (pendingTerms()) return '请先在“接入设置”区域阅读当前发行条款，自行决定是否同意；未同意不会调用模型。';
      if (activeBrowser()) return '测试浏览器正在准备，请等待完成后点击“重新检测（不调用模型）”。';
      if (state?.browser && !['ready', 'not_required'].includes(state.browser.status)) return '请先在“接入设置”区域“确认并准备测试浏览器”；失败时查看原因，再自行决定是否明确重试。';
      if (!state || state.client === 'unchecked') return '请先在“接入设置”区域点击“重新检测（不调用模型）”。';
      if (state.client === 'missing') return '未找到本机 Claude Code，请按上方官方说明自行安装，再重新检测。';
      if (state.compatibility !== 'supported') return 'Claude 版本或必需参数未通过检测，请查看上方接入状态，处理后重新检测。';
      if (state.dependencies !== 'healthy') return '本地依赖尚未通过自检，请查看上方接入状态，确认完整便携包及测试浏览器后重新检测。';
      if (state.login === 'logged_out') return 'Claude 尚未登录，请自行运行 claude auth login，再点击“重新检测（不调用模型）”。';
      if (state.login === 'unknown' && !state.existingConfirmed) return '登录状态未知。确认本机配置可用后，可在“接入设置”区域自行选择“使用已有本机配置继续（接入未验证）”；不会自动确认或调用模型。';
      return '接入尚未就绪，请查看“接入设置”区域首次接入状态。';
    }
    function controls() {
      recheck.disabled = confirmExisting.disabled = prepareBrowser.disabled = expired || busy || activeBrowser() || pendingTerms();
      acceptDistribution.disabled = exit.disabled = expired || busy || activeBrowser();
    }
    const bytes = value => Number.isFinite(value) && value >= 0 ? (value / 1048576).toFixed(1) + ' MiB' : '未知';
    function update(value) {
      state = value;
      const blocked = linuxBlocked();
      if (blocked) {
        title.textContent = 'Linux服务器控制台 · 执行能力尚未验证';
        install.textContent = '1. 打开官方 Linux 安装说明';
        installHelp.textContent = '请在服务器上使用匹配CPU架构的原生组件；控制台不会自动安装依赖、下载浏览器或修改全局配置。';
        second.textContent = '2. 当前为受限控制台：可以保存项目和需求，模型操作始终禁用。root部署不代表浏览器允许关闭沙箱。';
        third.textContent = '3. 重新检测仅刷新当前门禁状态；完整受控执行器、原生依赖和浏览器沙箱仍待验证，不调用模型。';
        note.textContent = '关闭浏览器不会停止服务器。通过SSH重新获取一次性访问链接；退出使用页面按钮或Linux入口shutdown，不删除目录锁。';
      }
      const terms = value.distributionTerms;
      distribution.hidden = !terms?.required;
      acceptDistribution.hidden = !pendingTerms();
      distributionText.textContent = terms?.text || '';
      distributionStatus.textContent = terms?.accepted ? '已在本机确认当前版本；这不是模型接入或分发许可审核结论。' : '未确认：不会执行依赖检测、下载浏览器或调用模型；不同意可以退出。';
      if (pendingTerms()) distribution.open = true;
      const names = { unchecked: '未检测', found: '已找到', missing: '缺失', supported: '支持必需参数', unsupported: '版本不兼容', unknown: '未知', logged_in: '登录已报告', logged_out: '未登录', healthy: '本地自检通过', unhealthy: '本地自检失败', fixture: '免费测试边界' };
      summary.textContent = `客户端：${names[value.client] || value.client} / 版本：${value.version || '未知'} / 参数：${names[value.compatibility] || value.compatibility} / 登录：${names[value.login] || value.login} / 依赖：${names[value.dependencies] || value.dependencies}。实际模型接入未验证。`;
      detail.textContent = value.message;
      confirmExisting.hidden = !(value.login === 'unknown' && value.compatibility === 'supported' && value.dependencies === 'healthy' && !value.existingConfirmed);
      const browser = value.browser;
      const required = !!browser && browser.status !== 'not_required';
      browserStatus.hidden = !required;
      browserNote.hidden = !required || blocked;
      prepareBrowser.hidden = !required || blocked || browser.status === 'ready';
      prepareBrowser.textContent = browser?.status === 'failed' ? '明确重试准备测试浏览器' : '确认并准备测试浏览器';
      const stages = { unavailable: '尚未验证，禁止下载与执行', missing: '首次测试浏览器未准备', downloading: '正在下载', verifying: '正在校验大小及 SHA256', extracting: '正在解压', checking: '正在本地自检（不调用模型）', ready: '本地准备完成，请重新检测 Claude', failed: '准备失败；未自动重试' };
      browserStatus.textContent = required ? `浏览器 ${browser.version}：${stages[browser.status] || '状态未知'}；已下载 ${bytes(browser.downloadedBytes)} / ${bytes(browser.totalBytes)}。${browser.message || ''}` : '';
      controls();
      changed(!!value.canExecute && !activeBrowser() && !pendingTerms(), executionHint());
    }
    let loading = null;
    function load(options = {}) {
      if (expired || busy) return Promise.resolve();
      if (loading) return loading;
      const expected = generation;
      loading = (async () => {
        try {
          const value = await api('/api/setup', undefined, false, options);
          if (!expired && !busy && !options.signal?.aborted && expected === generation) update(value);
        } catch (error) {
          if (!expired && !busy && expected === generation) { summary.textContent = '接入状态读取失败：' + error.message; changed(false, '无法读取接入状态。请查看“接入设置”区域错误；会话失效时双击 start-workbench.cmd 重新打开。'); }
          throw error;
        }
      })().finally(() => { loading = null; });
      return loading;
    }
    acceptDistribution.addEventListener('click', async () => {
      if (busy || !pendingTerms() || !confirm('确认你已阅读并同意页面展示的当前 Windows 发行条款？只记录本机文档摘要和时间，不调用模型；不同意可取消并退出。')) return;
      const checksum = state.distributionTerms.sha256;
      generation++; busy = true; controls(); changed(false);
      try { update(await api('/api/setup/distribution', { confirmed: true, sha256: checksum })); }
      catch (error) { detail.textContent = '条款确认未完成：' + error.message; }
      finally { busy = false; controls(); }
    });
    prepareBrowser.addEventListener('click', async () => {
      if (busy || activeBrowser() || !confirm('确认从 Playwright 官方 HTTPS 源下载固定版本测试浏览器，并已阅读浏览器适用条款？约173 MiB，仅存放在工作台文件夹，不安装到系统，不调用模型，失败不自动重试。')) return;
      generation++; busy = true; controls(); changed(false);
      try { update(await api('/api/setup/browser', { confirmed: true })); }
      catch (error) { detail.textContent = '浏览器准备被拒绝：' + error.message; }
      finally { busy = false; controls(); }
    });
    recheck.addEventListener('click', async () => {
      if (busy) return;
      generation++; busy = true; controls();
      detail.textContent = '检测中：Claude固定命令每项最多10秒、合计128KiB；另运行本地依赖和浏览器自检（最多30秒）。不启动reason守护进程、不调用模型。';
      changed(false);
      try { update(await api('/api/setup/recheck', {})); }
      catch (error) { detail.textContent = '检测失败：' + error.message; }
      finally { busy = false; controls(); }
    });
    confirmExisting.addEventListener('click', async () => {
      if (busy || !confirm('我了解登录状态未知，使用自己已有的本机配置继续；后续真实任务会调用模型并可能计费，接入尚未验证。确认继续？')) return;
      generation++; busy = true; controls();
      try { update(await api('/api/setup/confirm-existing', { confirmed: true })); }
      catch (error) { detail.textContent = error.message; }
      finally { busy = false; controls(); }
    });
    exit.addEventListener('click', async () => {
      if (busy || !confirm('退出本机后台？活动、排队、暂停或未知租约会被拒绝；不会替你终止任务。')) return;
      try { await api('/api/shutdown', {}); summary.textContent = '已请求正常退出；此页面将失去连接。'; exit.disabled = true; changed(false); shutdown(); }
      catch (error) { detail.textContent = '退出被拒绝：' + error.message; }
    });
    return { load, stop() { expired = true; generation++; controls(); changed(false, '页面会话已失效或正在退出，请通过原启动器重新打开。'); } };
  }
})();
