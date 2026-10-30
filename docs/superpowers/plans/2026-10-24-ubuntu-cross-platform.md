# Ubuntu 无桌面双架构适配 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 在 Ubuntu Server 24.04 LTS x86_64 与 ARM64 上提供同一工作台的原生无桌面、安全生命周期和 SSH 访问方式，每平台以实际证据独立报告。

**Architecture:** 沿用前一计划完成的业务状态机与前端，抽取进程、锁和实例通信这几个 OS 边界。Windows 保留 Job Objects；Ubuntu 以 systemd 用户服务与 cgroup v2 接管所有受控子命令，双 loopback 端口通过 SSH 等端口转发。两个 Linux 包分别锁定原生架构依赖，使用 Ubuntu 系统 Python 3.12，不仿造 Windows portable 环境。

**Tech Stack:** Ubuntu 24.04/systemd/cgroup v2、Python 3.12 标准库、SQLite、Node 24.14.0、Playwright/core 1.58.2、reason-network 0.10.8；具体可分发归档的来源/哈希在 Task L1 核实，未核实不得构建发布。

**Spec:** `docs/superpowers/specs/2026-10-24-workspace-stability-design.md` 第10节及全部安全不变条件。前置实施计划：`docs/superpowers/plans/2026-10-24-workspace-stability.md`，不复制其状态/回执/UI实现。

## Global Constraints

- Ubuntu Server 24.04 LTS，x86_64（amd64）和 ARM64（aarch64）；不承诺32位或其他发行版。
- 当前在线 Windows 源码/58593项目不改；沿用前一计划的隔离 DEV，新平台代码仍只在 DEV 内开发。
- 不创建 worktree、不 commit/push、不安装/启用VM、不连接记忆中的业务主机、不擅自租云。
- 无桌面/无DISPLAY不等于不需要服务器端浏览器；独立测试与可信复跑都必须真实运行 headless Chromium。
- 不以普通进程组、父PID退出或shell的exit0替代完整树停止证明；不能确认就 blocked/leaseUnknown。
- 单用户普通非root、当前UID控制；不默认sudo/apt/loginctl、自启、linger或修改AppArmor/sysctl。
- 固定control 8787与preview 8788为Linux默认，可显式换成另一对；两者仅127.0.0.1，转发客户端端口必须与服务器端口相同。
- 不开放公网、不放宽Host/Origin/CORS/CSP，不复制条款同意或Claude认证。
- 每架构独立包/哈希/原生实测报告，QEMU/WSL/容器不能冒充干净原生服务器验证。
- 真实模型授权仍总共一次；Linux额外真实闭环需要新的明确费用授权；不得自动付费重试或代人工验收。
- 任务均先RED再实现，所有能力不足与未测平台明确报告；不通过把测试全skip制造兼容。

## Review Focus

1. 父程序正常退出但setsid孙进程还持有文件：完整cgroup未空前不能释放租约；L3覆盖。
2. 宿主SIGKILL发生在unit启动与登记之间：旧命令不能成为无主写者或在恢复时漏查；L3/L4覆盖启动门与日志原子性。
3. SSH只转发控制端口：页面可开但iframe打不开不能称可用；L5双端口真实端到端覆盖。
4. X64机器误拿ARM包、执行位丢失、外部data root仍写BASE：L2/L6覆盖失败诊断和数据根隔离。
5. 临时接入文件被符号链接/其他UID访问或token落journal：L4/L5覆盖权限、peer credentials和日志检查。

---

## 文件地图和公共接口

- 新 `p1/platform_info.py`：仅标准库的平台/CPU/ELF检测，不启动外部程序。
- 新 `p1/root_lock.py`：平台选择的独占根锁；`p1/server.py`保留导入别名兼容旧测试。
- 新 `p1/process_control.py`：`JobProcess`平台选择；`p1/winjob.py`保留Windows实现，Linux导入不能执行它。
- 新 `p1/linux_job.py`：Linux cgroup受控进程，接口与现有JobProcess一致。
- 新 `p1/linux_service.py`：用户服务宿主、已验证HostContext、私有执行ledger与恢复核对。
- 新 `p1/linux_exec.py`：systemd内启动门、报告实际PID/时间、校验归属后exec目标；不是浏览器可访问的任意命令端点。
- 新 `p1/posix_instance.py`：Unix socket同UID实例控制；现有 `p1/instance.py`移为Windows实现的兼容门面或由新增 `p1/instance_control.py`按平台分派，具体采用后者，现有文件不重命名。
- 新 `p1/instance_control.py`：统一InstanceChannel/request，Windows原导出不变。
- 新 `p1/browser_catalog.py`：按锁定平台选择浏览器描述；`browser_setup.py`保留下载/校验流程并接收显式数据根。
- 修改 `environment.py/runtime.py/reason_mcp.py/checks.py/engine.py/bootstrap.py/server.py/distribution.py/setup.js/live.js/checks.cjs`：平台依赖/私有数据根/提示与生命周期接入。
- 新 `start-workbench.sh`、`LINUX-START-HERE.md`、`tools/build_linux.py`、`tools/run_linux_validation.py`。
- 新 `tests/p1/test_platform_info.py`、`test_posix_instance.py`、`test_linux_process.py`、`test_linux_startup.py`、`test_linux_packaging.py`、`test_linux_data_root.py`、`tests/p1/linux_lifecycle_fixture.py`、`tests/p1/ssh-browser.cjs`；现有Windows测试保留并正确标注OS专用skip。
- 新依赖锁 `packaging/linux-x64/dependencies.json` 与 `packaging/linux-arm64/dependencies.json` 只在L1来源核实后生成；不写猜测哈希。

Linux验证命令约定：Python `python3.12 -I -B -X utf8`；Node使用包内绝对路径；后端仍 `-m unittest discover -s tests/p1 -p 'test_*.py' -v`，JS和真实HTTP/browser沿用前一计划FULL。拥有外部进程的测试由 `tools/run_linux_validation.py` 在自身受控临时用户service内运行，并给子测试传经过核验的私有HostContext；不在普通终端模式下悄悄降级。

## Task L1: 两架构依赖与可用验证环境核实

**Files:**
- Create: DEV 的 `dist/linux-preflight/<new>/platforms.json`、来源/版本/许可证记录。
- Create after verified: `packaging/linux-x64/dependencies.json`、`packaging/linux-arm64/dependencies.json`。

**Interfaces:**
- 两lock固定 `format:3`、`platform:linux-x64|linux-arm64`、`systemPython:{major:3,minor:12}`；archives每条必须有来源URL、版本、架构、sha256、许可证、允许文件及可执行位清单。
- 固定Node24.14.0、Playwright/core1.58.2、reason0.10.8；浏览器来自同Playwright版本registry实际列出的对应平台，不要求x64和arm64归档布局相同。
- 事实记录每平台的 `assetsVerified`、`nativeEnvironmentAvailable`、`systemdAvailable`、`modelE2EAuthorized:false`，不能只有总通过布尔值。

- [ ] **Step 1: 核实官方资产。** 读取已固定Playwright包的registry、browsers.json、nativeDeps；可访问官方来源后核实两架构Node/reason原生归档、精确浏览器路径与许可证，分别记录SHA256。Linux浏览器归档在获得该平台真实条款/首次下载同意前仅核实公开元数据，不实际下载；若无可核验官方哈希，记录阻塞，待L6正常同意后计算，不填猜值。其他已获许可的构建输入下载到各自全新目录，脚本放另一目录，Python读取用-I；不执行下载到的脚本/安装器。
- [ ] **Step 2: 写锁定前断言。** 来源未能访问/资产缺失不得生成可构建锁；同名Windows文件不能充Linux、arm64不能充x64。若reason0.10.8无对应官方归档，停该平台发行准备，写明同版本源码可复现构建所缺工具链；不得自行换消息总线/伪造ACK或偷偷升版本。
- [ ] **Step 3: 核实授权环境。** 本机无已确认可用Linux环境，先记录这一事实。已有用户明确授权的新测试主机才可只读探测 `/etc/os-release`、uname、systemd用户manager/cgroup、Python/内存/磁盘/非root等；未提供主机不找全局SSH配置或记忆凭证。不具备环境时L3–L7真实验证保留阻塞，但L2跨平台静态/Windows可先做。
- [ ] **Step 4: 将已核实依赖清单和最小Ubuntu库清单写入lock/文档。** 精确包名从固定Playwright nativeDeps的ubuntu24.04映射获取并在目标apt仓库验证；用户自行批准执行安装，不运行apt/sudo。纯调查任务不新增产品代码，不虚称RED/GREEN或兼容通过。

## Task L2: 平台识别、可导入核心和独占锁

**Files:**
- Create: `p1/platform_info.py`、`p1/root_lock.py`、`p1/process_control.py`。
- Modify: `environment.py/runtime.py/server.py/reason_mcp.py/checks.py` 和持有JobProcess的测试runner。
- Test: `tests/p1/test_platform_info.py`、现有 `test_environment.py/test_startup.py/test_process.py`。

**Interfaces:**
- `platform_info.target(system: str | None=None, machine: str | None=None) -> str`：WindowsAMD64->win32-x64；Linuxx86_64/amd64->linux-x64；aarch64/arm64->linux-arm64；其他ValueError。
- `platform_info.native_executable(path: Path, target_name: str) -> Path | None`：Windows复用既有MZ要求；Linux先严格解析绝对候选路径，最终文件必须普通文件、有X权限、ELF64 little endian，e_machine=62或183与实际平台匹配。Claude官方安装器可能使用符号链接，因此发现阶段允许解析有效链接并返回规范绝对目标，拒绝悬空/循环和组或其他用户可写的候选链；包内依赖仍由bundle_file拒绝归档链接。不猜Claude路径，仅官方已核实路径/绝对PATH候选。
- `root_lock.RootLock(root)`、`root_lock.OwnedRoot` 与现有close语义一致；Linux使用fcntl.flock(LOCK_EX|LOCK_NB)，持有FD，权限0600；只把EAGAIN/EACCES当OwnedRoot，其余真实storage错误上抛。
- `process_control.JobProcess`：平台lazy导入winjob/linux_job；L3未实现时Linux明确能力不具备，不fallback到subprocess弱实现。
- `RuntimePaths`字段保持旧字段名兼容，Linux pythonWindowed=pythonConsole，不再把源码Python换成python.exe；Linuxsource要求显式已核实依赖，不搜索任意开发机路径。

- [ ] **Step 1: 写纯RED测试。** 覆盖正确/不支持架构、ELF错误机器/权限/截断、官方样式链接成功且返回规范路径、悬空/循环/可由其他用户修改的候选链拒绝；Linux路径不引用APPDATA，导入server/环境不能触发msvcrt、WinDLL或_winapi。`self.assertEqual(target('Linux','aarch64'),'linux-arm64')`、`self.assertEqual(target('Linux','x86_64'),'linux-x64')`；32位用assertRaises(ValueError)。Windows用字节样本验证ELF逻辑，不当真实CLI。
- [ ] **Step 2: 写Linux根锁RED。** 两真实进程竞争同目录，只有一个成功；锁文件存在但无owner可取得；持锁到恢复开始前；目录不可写不误报已拥有；符号链接/错误owner拒绝。测试目录root0700，不对NFS行为作支持承诺。
- [ ] **Step 3: 实现平台门面和锁，更新imports。** 保留server.RootLock/OwnedRoot别名；Windows winjob实现不改，环境health的标准库import改为平台对应（Linux不导入_winapi）。探测只是支持当前平台，不改变模型login未知的严格语义。
- [ ] **Step 4: 跑新纯单测和Windows FULL。** 之后真实Ubuntu运行同单测及根锁专项；无法运行时标未验证，不能用patch os.name当Linux运行证据。原Windows process/instance/startup测试OS skip放在导入副作用之前，但每个skip有对应Linux独立任务而非静默删测试。

## Task L3: Linux受控进程与宿主生命周期

**Files:**
- Create: `p1/linux_service.py`、`p1/linux_exec.py`、`p1/linux_job.py`、`tools/run_linux_validation.py`（先提供受控host执行与单套件选择，L7扩充汇总）。
- Test: `tests/p1/test_linux_process.py`、`tests/p1/linux_lifecycle_fixture.py`。
- Modify: `tests/p1/support.py`（仅测试unit上下文构造）。

**Interfaces:**
- `HostContext.load() -> HostContext`：从 `RELAY_HOST_DESCRIPTOR` 指向0600私有文件读取，核验当前UID/root/宿主unit/InvocationID/当前cgroup归属，单靠env字符串不能授权。调用者只能是宿主或ledger中当前仍活动的已登记执行unit；嵌套执行记录parentUnit/parentInvocationID并同时绑定调用者与宿主。非受控service模式可导入/只读，但禁止起真实子命令。
- `HostContext.preflight() -> dict`：检查systemd用户manager、cgroupv2、非root、控制组可核验、无DISPLAY下依赖；不修改主机。`tools/run_linux_validation.py --root <DEV> --output <new> --suite linux-process|linux-startup|linux-data-root|linux-packaging|all [--native-required]`在专属受控验证host中运行，套件固定映射unittest文件，不接受浏览器端命令或任意shell。L3先实现linux-process及host启动，L4/L6新增相应映射，L7完善all和最终矩阵。
- `HostContext.register(unit_name, argv_hash) -> descriptor`、`confirm_empty(descriptor) -> bool`、`recover() -> {confirmed:bool, unresolved:list}`；ledger位于data root `process-ledger/`，0700目录/0600文件，临时写+fsync+原子替换，并fsync目录。跨进程fcntl锁串行化register/stop，stop先持久化stopping再枚举后代；任何祖先stopping时register/启动门都拒绝，防止清理后又启动。每次宿主在派工前写host generation及完整登记起点，损坏/缺失登记不能推断无子执行。
- `LinuxJobProcess(argv,cwd,env=None,input_data=b'',output_limit=8*1024*1024,stdout_observer=None,wait_observer=None)` 与Windows同签名；poll/stop/tree_stopped/active_processes/wait/close同语义和现有result键，观察队列64、输出总上限、超时保持一致。
- 子unit名 `relay-child-<rootHash16>-<uuid32>.service`，宿主 `relay-host-<rootHash16>.service`；rootHash取规范绝对数据根+UID SHA256前16位。unit记录InvocationID和ControlGroup，防止名字/PID重用误清别的任务。
- 用 `systemd-run --user --pipe --wait` 创建Type=exec、ExitType=cgroup用户service，子unit设置BindsTo/PartOf/After=当前宿主；若调用者为已登记子unit，再同时绑定该调用者unit。KillMode=control-group、TimeoutStopSec=5s、SendSIGKILL=yes；不是先Popen目标再移动cgroup。JobProcess.tree_stopped除自身cgroup外，还须核对其ledger递归后代unit全停，因为reason调用等嵌套JobProcess可能创建不同cgroup，不能被漏算。
- ExecStart只调用包内可信 `linux_exec.py`；输入argv/cwd/env以0600本地handoff传递，不进systemd属性/普通日志，不开浏览器任意命令API。helper先验证UID/宿主和unit/cgroup绑定、持久化PID/startedAt，再等待父确认启动门（最长10s），收到绑定确认后execve目标。宿主死亡/门超时则退出，不执行目标。
- manager请求命令本身有10s有界deadline；stop按TERM后最多5s升级KILL，再最多5s验证cgroup.events populated=0/manager已停止及对应原cgroup撤销。manager不可达/路径身份不明，返回false而非猜已停止。

- [ ] **Step 1: 写并运行真实Ubuntu RED。** `test_grandchild_setsid_keeps_lease_until_group_empty`、`test_parent_exit_does_not_end_tree`、`test_sigterm_ignored_is_killed`、`test_stdin_stdout_stderr_drain`、`test_output_limit_terminates_cgroup`。先用非模型fixture写心跳/持文件，必须观察到未受控或接口缺失的预期失败，测试cleanup仍可通过测试owned unit停止本轮进程，不kill任意PID。
- [ ] **Step 2: 写启动竞争和宿主崩溃RED。** 在ledger预写、unit创建、helper ready、启动门确认各点杀测试宿主，目标不得逃逸；宿主SIGKILL时子unit全部停止，重启可根据ledger核对。`test_nested_owned_unit_stops_with_its_caller`使用fixture在子执行内再次创建JobProcess并退出外层，验证内层cgroup与全部IO也停止。单纯unitnotfound只有在核验manager/原InvocationID及cgroup消失后才可成为证明。
- [ ] **Step 3: 实现HostContext/helper/JobProcess。** 保留现有stdout observer缺口和backpressure语义。包装systemd-run进程exit0不代表目标exit0，从已验证unit/私有终结记录读取实际退出码；目标及所有后代全停且IO线程收束后才能treeStopped。父退出但子还活时继续等到deadline，不提前清理成成功。
- [ ] **Step 4: 写并运行能力不足RED→GREEN。** 缺user manager、非v2、root、错UID/InvocationID、ledger权限/符号链接、manager超时均明确拒绝执行；不存在弱fallback。私有handoff终止后擦除env/命令正文，ledger仅留绑定/哈希与停止状态。
- [ ] **Step 5: 在Ubuntu两原生架构分别跑L3专项和相关FULL。** Windows跑FULL确认门面未改变Job Objects；Linux缺环境则此任务不得标完成。只用真实非模型fixture，不启动付费CLI。

## Task L4: Unix实例重开、数据根和异常恢复接入

**Files:**
- Create: `p1/posix_instance.py`、`p1/instance_control.py`。
- Modify: `bootstrap.py/server.py/environment.py/browser_setup.py/distribution.py/engine.py`。
- Test: `tests/p1/test_posix_instance.py`、`test_linux_startup.py`、`test_linux_data_root.py`、现有environment/distribution/browser测试。

**Interfaces:**
- `instance_control.InstanceChannel(root, handler)`、`request(root,command,timeout=8)`，Windows继续使用原instance实现和probe/open-ui命令；Linux handler是接收白名单命令、返回非敏感有界dict的回调，支持probe/access/shutdown。新增shutdown仅调用Application.request_shutdown的既有门禁，不执行systemctl stop绕过活动租约。
- Linux socket `${XDG_RUNTIME_DIR}/relay-workbench/<rootHash16>/control.sock`，确认runtime dir真实属于当前UID且0700；整条sun_path<108字节，过长明确拒绝。socket0600，SO_PEERCRED校验UID；probe/access/shutdown白名单消息最大4096字节、单连接8s deadline，不允许任意文件/argv。
- `SetupStatus(bundle_root,paths=None,discover=None,probe=None,*,data_root=None)`；默认data_root=bundle_root/userdata，旧默认字节布局不变。`DistributionTerms(bundle_root,*,data_root=None)`；`BrowserSetup(bundle_root,required=False,descriptor=None,fetch=None,check=None,*,data_root=None)`同理。setup所有写入只在data_root；manifest/条款来自bundle_root。
- `resolve_environment(bundle_root,*,data_root=None)`、`source_or_bundle(root,*,data_root=None)`将显式data_root透传至浏览器路径。`browser_directory(bundle_root,*,data_root=None,descriptor=None)` / `browser_executable(...)` / `verified_browser(...)`用相同关键字兼容旧调用；Windows旧receipt仍在原userdata/browser位置，Linux不同descriptor不能复用。
- Application以`SetupStatus(BASE,data_root=self.store.root)`构造；Engine的CheckRunner使用`setup.paths`而不是模块级默认PATHS，RoleRunner新增关键字`paths=None`并透传给`cli_command(...,*,paths=None)`；ReasonTransport使用同一paths.reason。原测试/调用省略参数保留旧行为，所有生产执行以当前Application的实际paths为准，可信复跑也不得漏传。
- 子CLI及其MCP配置显式传递经过验证的RELAY_HOST_DESCRIPTOR，使reason_mcp在已登记调用者unit中核验并创建绑定的嵌套reason调用。descriptor路径不是bearer/token；MCP和子程序不据此获得更广的工具权限。
- Application额外可选 `preview_port=0`；Linuxbootstrap显式8787/8788，Windows默认仍0随机preview。Linux宿主创建任何数据前设置umask 0077，data root及tmp/private-access/process-ledger为0700、SQLite及MCP/receipt为0600；owned child的TMPDIR指向当前data root/tmp，检查临时数据也受该根约束。Windows现有ACL/路径行为不因POSIX模式改写。
- `linux_service.start(root,ports) -> {unit,alreadyRunning}` 在唯一根锁/同UID通道规则下启动或重开，不停止owner；宿主descriptor在服务环境传递，session/bearer不写其中。宿主ExecStart直接执行可信Python bootstrap，不套后台shell；Type=exec、ExitType=main、Restart=no、KillMode=control-group、TimeoutStopSec=90s、SendSIGKILL=yes，不能用ExitType=cgroup使宿主主进程死后仍因wrapper存活而维持active。SIGTERM handler仅设置退出事件，主线程执行原有有界quiesce/close；未全停不得释放根锁或记录停止成功。
- 恢复流程：取得RootLock→核验旧ledger/停止本root已绑定unit→Store按原逻辑recover为blocked；必须存在覆盖旧root宿主及全部子执行的可核验绑定且全停，才可逐项目用`Store.command(project_id,fresh_command_id,current_revision,'stop',{})`按原路径撤销批准，再用既有`Store.confirm_stopped(project_id,True)`标stopped。不新增直接DB变更，不把状态改draft/ready或恢复执行。没有ledger/旧版本无绑定、manager未知或证明不全，继续blocked/leaseUnknown；空ledger不是证明。

- [ ] **Step 1: 写并观察数据根RED。** manifest只读但dataRoot可写，条款/浏览器receipt/临时下载只落dataRoot，BASE/userdata不出现；两个隔离dataRoot互不复用同意；旧Windows默认布局仍精确相同。`test_external_root_browser_is_used_by_health_role_and_trusted_rerun`验证检测、角色run_check、宿主独立复跑使用同一外部root浏览器与paths，不能只测SetupStatus生成路径。
- [ ] **Step 2: 写Unix通道RED。** 同UID重开owner PID不变；权限/peerUID不匹配、链接路径、过长socket拒绝；占用锁但socket未ready时有界等候，不误起第二实例。清理socket仅在本root锁取得且验证自己创建时执行。
- [ ] **Step 3: 接入数据根和实例/恢复。** Linux入口不调用webbrowser/MessageBoxW；失败打印脱敏分类而非rawenv/token，异常cleanup有界。无证据不得调用confirm_stopped；对恢复项目先显式撤销残余task批准再标stopped，保持手动重新批准/开工门禁。
- [ ] **Step 4: 跑专项GREEN及旧suite。** 真实两进程启动100组唯一owner/重开/正常退出；有活跃/未知租约时HTTP退出仍拒绝。SIGTERM可清理，SIGKILL由systemd绑定清理并验证恢复；`test_missing_or_corrupt_ledger_keeps_recovery_blocked`断言未知绑定绝不清租约；`test_register_during_stop_cannot_start_new_writer`固定停止竞争。SSH断开不触发hostshutdown；用loginctl用户Linger属性及实际断线测试核实manager持续运行前提，不满足则启动前明确阻塞，不自动改设置。

## Task L5: 双端口SSH、安全接入链接与平台化提示

**Files:**
- Create: `start-workbench.sh`、`LINUX-START-HERE.md`。
- Modify: `linux_service.py/posix_instance.py/bootstrap.py/server.py/setup.js/live.js`。
- Create: `tests/p1/ssh-browser.cjs`，修改 `test_linux_startup.py/test_server.py`。

**Interfaces:**
- `start-workbench.sh start --root <absolute> --port 8787 --preview-port 8788`；`access --root <absolute>`；`status --root <absolute>`；`stop --root <absolute>`。默认root为包内userdata。shell必须定位自身目录，处理空格，不eval用户参数，不自动下载/提权，Python3.12不足给明确诊断。
- start使用受控user service且不自动启用/自启；stop通过已有正常shutdown门禁，活动任务拒绝，不等同systemctl kill。强制系统退出不作为UI正常停止入口。
- access经同UID私有socket请求新一次性bootstrap，Linux不在Application初始化时额外签发未请求的bootstrap；写入root下 `private-access/<uuid>/launch.txt`（0700/0600，O_EXCL/O_NOFOLLOW），TTL60s，过期/消费后清理；终端只输出非敏感origin、路径与访问方法，不打印链接本身。用户读取文件后用于其浏览器；journal无token。
- `/api/state` 增加非敏感 `platform`、`accessMode:'local'|'ssh'`，接入文字据此生成；旧后台缺字段默认Windows本地提示。浏览器安装链接、条款标题、体积和termsUrl来自已核实descriptor，不继续对Linux写Git for Windows/173MiB/Google CFT条件。
- 文档SSH示例使用 `ssh -N -o ExitOnForwardFailure=yes -o ServerAliveInterval=30 -o ServerAliveCountMax=3 -L 127.0.0.1:8787:127.0.0.1:8787 -L 127.0.0.1:8788:127.0.0.1:8788 <用户>@<服务器>`；保留占位符，不提供已知业务服务器地址/凭证。

- [ ] **Step 1: 写并观察真实双端口RED。** 专用测试SSH目标仅在用户提供/授权环境执行；访问者浏览器通过隧道打开，控制页和产物iframe均加载正确分源、sandbox仍限制、无远程外链。只开control时明确失败不能通过，重建双转发后只读恢复。
- [ ] **Step 2: 写安全RED。** 令牌单次/过期不可复用、裸URL API403、错误Host/Origin拒绝；私有目录/文件链接攻击或别UID读取拒绝；检索本轮journal/stdout无bootstrap/bearer/环境正文。检索只对本轮私有日志，不能全局暴露用户日志。
- [ ] **Step 3: 实现CLI/链接/文字。** 双端口占用明确失败，不随机换端口；access不重启host，先建隧道再索取避免过期；返回UI未连接不自动提交任何mutation。Linux错误提示指向shell access而不是双击CMD。
- [ ] **Step 4: 运行真实SSH浏览器GREEN和FULL。** 断线/重连/会话更新时DB command/attempt/project数不增长；375/1440截图实际查看。无SSH测试环境则该项未验证，不在Windows本地localhost截图代替。

## Task L6: 架构锁定的运行时、浏览器准备与Linux包

**Files:**
- Create: `p1/browser_catalog.py`、`tools/build_linux.py`。
- Modify: `environment.py/browser_setup.py/checks.cjs`、`tools/build_portable.py`（仅需要保留共享源码清单时）。
- Create: `tests/p1/test_linux_packaging.py`；修改browser/environment/checks回归。

**Interfaces:**
- `browser_catalog.descriptor(platform_name,lock) -> dict`：version/revision/url/termsUrl/sha256/bytes/executableRelativePath；严格由L1锁给出，不接受浏览器传url/路径。
- resolve_environment支持format3的平台校验；Linux `files`包含node/reason/playwright，Python来自实际运行的系统3.12；chromium按L4的dataRoot和descriptor解析，绝不fallbackWindows或任意系统Chrome。L4/L5在真实归档核实/下载前可用明确标注的最小平台fixture验证接口；完整格式3依赖和真实浏览器能力只在本任务验收，前置任务不能假称已通过。
- `tools/build_linux.py --source <DEV> --dependencies <verified-input> --platform linux-x64|linux-arm64 --output <new-dir>` 输出干净目录、portable.json format3、licenses、inventory、tar.gz和SUMS；不包含userdata/认证/同意/模型CLI/测试证据。
- 归档每个普通文件只允许清单声明的0644或0755，不保留setuid/setgid/owner；拒绝绝对/..路径、符号/硬链接、设备节点和归档炸弹；需官方链接资产先安全重排并重新完整性核验，不能全面允许链接。
- `checks.cjs` Linux启动必须 `chromiumSandbox:true`（Windows行为保留），不传--no-sandbox，检查实际sandbox可用。不使用root/Xvfb/disable验证绕过；临时profile归owned cgroup并清理。

- [ ] **Step 1: 写并观察纯构建RED。** 两架构lock/ELF相反、缺可执行位/库/错误哈希、链接越界/setuid/重复成员不通过；缺官方哈希L1阻塞不被fallback；用合成无可执行payload归档验证构建器，不将fixture当许可/在线下载证明。
- [ ] **Step 2: 实现manifest/构建。** 共用相同业务源；Windowsbuild仍只输出win32-x64，默认行为不变。新增Linux脚本和三个live模块均纳入完整清单，依赖与第三方通知分别可追溯。
- [ ] **Step 3: 写浏览器数据根/权限/下载RED。** 新归档具体内容按锁验证；下载同意在正常UI，固定官方HTTPS、大小/sha、原子安装、inventory和真实browser health；缺系统库不能半安装标ready。`test_linux_private_files_are_not_group_readable`检查0700/0600；`test_download_disk_full_leaves_no_ready_receipt`用测试专属fetch在本轮隔离目录抛OSError(errno.ENOSPC)，断言failed/disk分类、无ready receipt、旧目录未覆盖，不填满真实主机磁盘。系统库安装仍由部署者显式执行。
- [ ] **Step 4: 在无DISPLAY非root有sandbox条件真实运行声明式检查及独立可信复跑。** 使用非模型fixture产品，断言报告/截图/trace/退出码/产品与spec哈希、树停止。ARM原生浏览器必须能跑，不将正确ELF头或--version作为浏览器验证。
- [ ] **Step 5: Windows FULL与两个Linux包专项GREEN。** 错误OS/架构报告准确；旧发行条款bytes不改，Linux新条件未经用户决定不注入接受记录。

## Task L7: 最终原生矩阵、故障循环与交付报告

**Files:**
- Modify: `tools/run_linux_validation.py`。
- Create: 两平台独立输出内的 `TEST-REPORT.md` / `verification.json`。
- Modify: `LINUX-START-HERE.md`、`README.md`、新包的离线手册。

**Interfaces:**
- runner `--root <extracted> --output <new> --native-required`，记录OS/CPU/非root/Python/工具哈希/cgroup/UID/是否模拟/是否有DISPLAY；每任务结果pass/fail/blocked/skip及原因，不只有总数。
- 将FULL各子进程放受控验证host unit内，源码/包的生产代码不可为测试另加后门；测试专用fixture/私有文件在独立output中。
- Windows、Ubuntu x64、Ubuntu ARM64报告各自区分：免费回归、真实模型闭环、干净环境、人工验收、许可证/发布审核，禁止由一个总“稳定”掩盖未验项目。

- [ ] **Step 1: 最终两个tar.gz独立解压。** 所有成员/权限/sha与manifest和上游覆盖对照；不从已有测试目录启动冒充最终包。测试文档命令从不同cwd/空格路径运行。
- [ ] **Step 2: 各自干净原生Ubuntu执行FULL。** 仅安装文档列明且获得许可的依赖；无桌面、无DISPLAY、正常用户、sandbox有效。Windows专用测试有说明skip，Linux对应专项必须实际执行；报告任何红项。
- [ ] **Step 3: 各平台100组启动竞争、30轮多项目/刷新/SSH中断、10轮fixture子树退出/宿主异常恢复。** 每轮验证唯一owner、没有残留写者/浏览器/reason、GET不新增mutation、cgroup/线程与DOM留存有界；不把有限循环变绝对保证。
- [ ] **Step 4: 额外真实模型关口单独处理。** 未取得Linux额外费用授权时不调用generate/start等真实模型；仅标免费流程通过，模型端到端未验证。此前Windows一次付费授权不得在这里每架构各用一次。
- [ ] **Step 5: 独立审查改动与证据，修复必须RED→GREEN再跑FULL。** 不只信子agent结论或语法检查。最终交付两个Linux包、Windows更新包及三个独立报告；未具环境平台保留blocked并说明所需主机，而不是标兼容完成。
- [ ] **Step 6: 用户验收与在线更新分别征得决定。** 展示实际历史项目入口、持久反馈、计数器真实产物/报告；不代点击accepted。不未经许可把源码复制回58593目录或操作其userdata。

## 自查与覆盖（计划阶段）

- Spec10.1/10.5支持/依赖/架构与许可证：L1/L2/L6。
- Spec10.2平台边界/根锁/同UID：L2/L4；Windows原行为回归贯穿。
- Spec10.3生命周期/停止证明/异常恢复：L3/L4/L7；无弱fallback。
- Spec10.4双端口SSH/短期capability/无桌面：L5；不改安全模型。
- Spec10.5显式数据根/安全解压/正常同意：L4/L6。
- Spec10.6原生环境/完整矩阵/额外费用：L1/L7；不可用就阻塞。
- 全部Review Focus已分配实际RED，不以模拟结果充真实生命周期。尚未具备的外部环境和官方哈希是明确前置门槛，不是可由实施者猜定的常量。

## 验证命令与执行顺序

- L2纯测试：`python3.12 -I -B -X utf8 -m unittest discover -s tests/p1 -p test_platform_info.py -v`；期望目标用例全部执行且0失败，OS专有skip另列。
- L3生命周期：`python3.12 -I -B -X utf8 tools/run_linux_validation.py --root <DEV> --output <new> --suite linux-process --native-required`；L4分别用linux-startup/linux-data-root，L6用linux-packaging。每次output为全新目录。RED先观察缺失行为断言失败；GREEN要求对应实际用例全部执行、0失败、所属cgroup及观察线程全部停稳，单纯runner启动失败不算RED。
- L5真实SSH：测试runner在授权服务器启动隔离宿主/准备无模型fixture后，访问者机器使用其已核实Node和Playwright运行`node tests/p1/ssh-browser.cjs`；测试参数从0600私有配置读取服务origin/浏览器路径及短期接入文件，不在argv/stdout传token。只有真实双转发及服务器端检查均成功才记通过，配置不具备则blocked。
- L7最终完整矩阵：`python3.12 -I -B -X utf8 tools/run_linux_validation.py --root <final-extracted> --output <new> --suite all --native-required`；报告逐项结果和skip/blocked，不能仅以runner exit0表示所有门槛满足。
- 与前一计划“两个计划的执行顺序与付费关口”一致：Windows先免费候选、平台共享代码变更后重新FULL及重建最终Windows包、最后才消耗一次Windows真实模型授权。若Linux阻塞而冻结Windows先交付，后续Linux改动用另一独立副本，真实模型证据不得跨版本继承。

## 执行方式

与前一计划一起由用户审阅。推荐Native顺序执行，先完成共用稳定性任务，再做平台边界；实现中到L1缺少环境/资产时保留可核实的阻塞，不为通过而调用用户未授权资源。任务结束保存文件hash与测试checkpoint，不进行skill示例中的Git提交。
