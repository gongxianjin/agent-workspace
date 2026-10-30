# 工作台稳定性与易用性 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让当前工作台的项目可找回、进度与操作结果可信，并在请求超时/刷新时防止重复写入和重复模型执行。

**Architecture:** 保留 Python/SQLite 的权威状态，新增只读提交回执和兼容的项目创建幂等性。把前端有界传输、页面数据、待确认操作拆为三个小模块，由现有 live.js 连接 UI；服务器固定拼接脚本，历史静态 HTML 不变。先在独立源码副本完成免费验证及新 Windows 包，之后执行另一份 Ubuntu 计划，共用业务和界面而不复制状态机。

**Tech Stack:** 原生 JS/CSS、Python 标准库、SQLite、现有 Playwright 1.58.2、现有 Windows Job Objects；无框架迁移。

**Spec:** `docs/superpowers/specs/2026-10-24-workspace-stability-design.md`（2026-10-24 用户已批准设计；本实施计划与执行方式待确认）。

## Global Constraints

- 优先级：数据与执行安全 > 操作状态可信 > 项目找回 > 布局美化。
- 不重启或热更新当前 58593 实例，不读写其实际项目/SQLite；不修改其正在服务的源代码文件。
- 不创建 worktree，不 commit/push，不动原 Git 删除、data、其他业务目录或 F1/demo 原字节。
- 开发目录必须是独立短路径；交付为独立新包，不能覆盖现有 r3。
- 不改 CAS、逐项批准、手动开工、独占租约、冻结证据、可信复跑、会话或条款门禁。
- 所有自动重试只允许 GET；POST 超时不等于失败/取消，也不允许自动重放。
- 最小待确认描述可放 sessionStorage，原始需求、上传正文和新 bearer 副本不得放进去；需求草稿只在内存。
- 一次新的真实模型最小闭环授权总数仍为一次，失败不自动付费重试；ready 不等于 accepted。
- 正式更新真实 userdata、停止用户实例、Linux 新条款/下载、额外付费模型和虚拟化/云资源均保留各自许可边界。
- 每个任务先观察针对缺失行为的 RED，再写最小生产实现，再 GREEN 和完整相关回归；每任务保存散列/测试记录而不是提交 Git。

## Review Focus

1. 当前项目已创建但旧列表迟到：不能抹去项目或跳回旧选择；Task 4 的乱序列表测试固定该行为。
2. Store 已提交而后续 stop/响应输出出错：不能返回“确定未提交”并诱导重复付费；Task 2 的提交后错误测试固定该行为。
3. sessionStorage 在初始化/写入/配额满时抛错：页面不能崩溃或偷偷丢掉未知提交；Task 3、5 覆盖读取和写入失败。
4. 跨标签修改时本地草稿虽失焦仍未保存：不得套用最新 revision 强行覆盖；Task 4、6 固定原始 CAS 基线。
5. 下载、遥测封存或父进程退出：均不能被描述成项目成功/磁盘保存成功；Task 7 覆盖语义和过期遥测。

---

## 文件结构、执行目录与公共约定

以下路径均相对**独立开发副本** DEV，不是当前运行实例所在的原目录 BASE。

- `p1/state.py`：创建去重与只读回执；`p1/server.py`：认证路由、确定拒绝分类、固定脚本拼接。
- 新 `live-session.js`：有界 fetch、错误分类、安全存储、single-flight GET 轮询；可在 Node require。
- 新 `live-workspace.js`：按 revision 合并项目、草稿、选择、按项目反馈、进度说明；可在 Node require。
- 新 `live-operations.js`：一次性 POST 提交、最小 pending 描述和只读回执恢复；可在 Node require。
- `live.js`：DOM、导航、按钮、上传/下载与上述模块的编排；`setup.js`：独立接入状态同步；`live-output.js`：绑定安全的遥测摘要回调。
- `styles.css`：只追加 `body[data-mode="live"]` 限定样式。
- `tools/build_portable.py`：纳入三个新 JS 文件；不更改旧包。
- 新 `tests/live-session.test.cjs`、`tests/live-workspace.test.cjs`、`tests/live-operations.test.cjs`：Node 原生 test。
- 新 `tests/p1/test_receipts.py`、`tests/p1/stability-browser.cjs`、`tests/p1/run_stability_browser.py`：真实 Store/HTTP/Chromium 回归。
- `tests/p1/support.py`：仅测试使用的确定性等待/故障门；不得把故障开关加到生产 HTTP。

命令约定（Windows Git Bash）：`PY` 指现有可信 Python 的绝对路径，`NODE` 指现有可信 Node，`DEV` 指隔离副本；每次命令显式指定工作目录或绝对路径。示例 `"$PY" -I -B -X utf8 -m unittest discover -s "$DEV/tests/p1" -p test_receipts.py -v`。执行器先从 `environment.source_or_bundle(DEV)` 读取/记录实际工具路径，不能凭空假设全局安装。

运行所有免费回归的集合 FULL：后端 `unittest discover -s tests/p1 -p 'test_*.py' -v`；Node `--test tests/*.test.cjs`；`tests/p1/run_browser.py`；`tests/p1/run_onboarding_browser.py`；`tests/p1/run_stability_browser.py`；`tests/run_static_browser.py`。均在 DEV 中，Python 使用 `-I -B -X utf8`，子进程树必须确认停止。新增测试后不硬编码原先 171/19 等总数。

## Task 1: 创建不影响在线工作台的开发及证据副本

**Files:**
- Create: DEV 下的 `dist/stability/baseline.json`、`dist/stability/checkpoints/`。
- Copy unchanged: 根目录现有应用/文档/启动器、`p1/`、`tests/`、`tools/`；仅复制 `dist/p1/reason-contract-sample.json` 这一非私密协议 fixture。
- Do not copy: userdata、旧 dist 树、vendor/portable.json、会话/启动私件或同意回执。

**Interfaces:**
- Consumes: BASE 现有源文件与已校验 reason 原生程序。
- Produces: DEV 绝对路径、文件基线、实际 Python/Node/Playwright/browser/reason 路径；后续任务仅改 DEV。

- [ ] **Step 1: 先建立隔离副本。** 用可信 Python `-I` 的显式 allowlist 复制到全新短临时目录 `relay-dev-<random>/workbench`，不使用 git worktree。在其父目录提供已校验 reason 的同布局副本，以满足当前 source 模式相对路径；不修改生产环境解析来便利测试。
- [ ] **Step 2: 验证隔离。** 断言 DEV 无 userdata/portable.json/会话/同意记录，源文件逐项哈希一致；开发 source 路径解析得到存在的真实工具，浏览器证据路径小于 Windows 248 UTF-16 单位。失败则只调整新副本位置/布局，不改 BASE。
- [ ] **Step 3: 跑 FULL 中当前已经存在的套件，记录 baseline。** 新稳定性套件尚未创建，明确记为不存在而不是通过。保留任何失败/异常；若基线红，先定位，不能无声降低断言。
- [ ] **Step 4: 保存隔离与基线记录。** 输出可公开的路径/哈希/测试状态，不输出 bootstrap、bearer 或完整 userdata。此任务只复制既有代码，不新增产品行为，后续任务开始执行 TDD。

## Task 2: 持久化创建幂等性与受保护只读回执

**Files:**
- Modify: `p1/state.py`（Store 初始化、create_project、command 附近）。
- Modify: `p1/server.py`（/api/projects、/api/command、异常处理及新增 receipt GET）。
- Create: `tests/p1/test_receipts.py`。
- Modify: `tests/p1/test_server.py`。

**Interfaces:**
- `Store.create_project(name: str, command_id: str | None = None) -> dict`：旧调用兼容；同 ID/同名称指纹返回原创建响应，同 ID/不同名称 Rejected。
- 新表 `project_creations(id TEXT PRIMARY KEY, fingerprint TEXT NOT NULL, project TEXT NOT NULL, response TEXT NOT NULL, created_at REAL NOT NULL)`；只在事务内与 projects 同时写入。
- `Store.command_receipt(project_id: str, command_id: str) -> dict`、`Store.creation_receipt(command_id: str) -> dict`。
- GET `/api/receipts?scope=project&project=<id>&command=<id>` 或 `?scope=create&command=<id>`，不接受重复/额外 query 字段。
- 回执公共形状 `{found, scope, commandId, projectId}`；找到时另含 `{acceptedRevision, epoch, attemptId}`，创建回执 attemptId=null。找不到时 projectId 为所查项目或 null。无需求/正文/完整历史响应/指纹/宿主路径。
- 原 commands 无需改写：从已保存 response 提取 revision、epoch 和该响应 activeAttempt.id；回执只说明接受，不等同于当前状态或进程停止。
- 已确定的提交前错误响应增加 `outcome:"rejected"`；提交后错误使用 500、`outcome:"unknown"`，后台已提交的动作仍有回执。旧客户端继续能读 error 字段。

- [ ] **Step 1: 写 RED。** `ReceiptTests.test_create_replay_is_one_project_and_changed_name_rejects`、`test_receipt_is_read_only_and_excludes_payload`、`test_old_database_upgrade_preserves_existing_records`；断言示例：

```python
first = store.create_project('同名项目', command_id='create-1')
assert store.create_project('同名项目', command_id='create-1') == first
assert len(store.list_projects()) == 1
with self.assertRaises(Rejected):
    store.create_project('另一名称', command_id='create-1')
receipt = store.creation_receipt('create-1')
assert receipt['projectId'] == first['id']
assert set(receipt) == {'found', 'scope', 'commandId', 'projectId',
                        'acceptedRevision', 'epoch', 'attemptId'}
```

其他断言：同名称不同 ID 仍可创建；并发同 ID 仅一个项目及目录；无效 ID 长度（空/101字符）拒绝；DB 插入失败回滚且不误发回执；只清理本次尚未提交的自有空目录，不触碰未知目录。旧 DB 用真实旧三表/记录/证据 bytes 构造，初始化后逐记录相等。
- [ ] **Step 2: 跑 receipts 测试观察 RED。** 必须因缺少接口/参数/行为失败，不是工具或导入失败。
- [ ] **Step 3: 实现 Store 接口。** 同一 `BEGIN IMMEDIATE` 中先查创建 ID 再生成项目/记录；保留旧 `{name}` 行为。receipt 不 claim、recover、enqueue 或持有 HTTP action_lock；项目不存在明确拒绝，不能跨项目查询。
- [ ] **Step 4: 写并观察 HTTP RED。** `test_receipt_requires_auth_exact_origin_and_scope`、`test_receipt_missing_while_post_waits_is_not_rejection`、`test_committed_stop_error_stays_unknown_with_receipt`。真实持有 action_lock 时发送 POST，GET 仍可返回 found=false；放锁后确有一个命令。stop 提交后注入测试 engine.stop 异常，状态已变且 GET 回执找到，HTTP 不返回确定拒绝。追加旧 `{name}`、缺 token、错误 Origin、重复 query、超长 query（>1024字节）及正文不泄露断言。
- [ ] **Step 5: 实现 HTTP。** projects 只允许 name 与可选 commandId；标记 Store 调用已提交后，后续异常不得误分类。读取回执无需接入检测，不触发模型；所有 auth/Host/Origin 保护沿用。
- [ ] **Step 6: 跑 receipts、server、state 及完整后端回归 GREEN。** 保存测试输出和本任务文件哈希，无 commit。

## Task 3: 有界请求与互不阻塞的只读轮询

**Files:**
- Create: `live-session.js`、`tests/live-session.test.cjs`。
- Modify: `live.js`、`setup.js`、`live-output.js`、`p1/server.py`、`tools/build_portable.py`。
- Modify: `tests/p1/test_server.py`、`tests/p1/test_packaging.py`。

**Interfaces:**
- UMD `RelaySession.createApi({getToken, fetchImpl=fetch}) -> api(path, value?, raw=false, options={})`。
- `options.signal` 与内部 deadline 合成，外部 abort 不被改写成 HTTP 拒绝；完成/失败均清 timer/listener。
- `RequestError`：`category` 为 http/transport/timeout/parse/aborted，另含 status、definitiveRejected（只有可信的提交前拒绝才 true）。
- deadline：state/setup GET/telemetry/session/receipts 10000ms；普通 POST、document 下载 30000ms；setup recheck 120000ms。
- `RelaySession.createPoll({read, success, failure, intervalMs=1200, maxDelayMs=15000}) -> {start, stop, wake}`：最多一个在途；失败间隔依次2400/4800/9600/15000ms；成功归1200；认证403停止，外部重新认证才恢复。
- `RelaySession.safeStorage(getStorage = () => globalThis.sessionStorage) -> {get, set, remove, available}`，在内部获取storage对象，连sessionStorage属性getter本身抛错也捕获；get/set/remove每次异常均可内存回退。所有页面模块共享这个wrapper，available=false时显式提示；token仍只使用原sessionStorage键或本标签内存，不新增持久化位置。
- `RelaySetup.mount(...)` 保留 load 返回 Promise；项目 poll 不再等待 setup.load；setup 定时读取独立。
- `/live.js` 响应固定按 `live-session.js; live-workspace.js; live-operations.js; live.js` 顺序拼接（仅纳入已实现文件；最后提交必须四个齐全），以 `;\n` 分隔；不 eval、不动态用户路径、不修改 index.html。

- [ ] **Step 1: 写并跑 Node RED。** `deadline_bounds_fetch_and_body_read`：fetch 与 response.json/blob 任一悬挂都在期限 reject；`external_abort_cleans_listener`；`poll_never_overlaps_and_backoff_is_bounded`；`post_is_sent_once`；`storage_throws_during_get_or_quota_set`。用可控时钟/传输替身测试边界，断言实际发送次数和 timer 清理，不只断言 mock 配置。
- [ ] **Step 2: 实现 RelaySession。** deadline 必须覆盖完整响应读取，不只是响应头。未知 POST 不能自动再发；认证失效和暂时网络问题分类分开。
- [ ] **Step 3: 添加 HTTP/打包 RED 并实现固定脚本响应。** 断言 Content-Type、CSP 不变、顺序固定、无任意文件入口，三个模块都入新清单；file:// 静态演示无需新模块也不调用真实控制器。
- [ ] **Step 4: 接入独立 polls。** 初始化先交换 bootstrap，再独立启动 state/setup；setup 失败立即撤销 ready，但不回滚已读项目。遥测使用同一有界 api，失败不撤销后台阶段；shutdown/unload 清理 polls。
- [ ] **Step 5: 跑 Node、server、packaging 和现有 HTTP 浏览器回归 GREEN。** 稍后 Task 8 还需真实浏览器慢 setup/断线验证；本任务不宣称仅凭传输单测完成端到端。

## Task 4: 单一页面数据模型、版本安全合并和内存草稿

**Files:**
- Create: `live-workspace.js`、`tests/live-workspace.test.cjs`。
- Modify: `live.js`。

**Interfaces:**
- `RelayWorkspace.create({storage}) -> workspace`。
- `workspace.ingest(projects, readSequence)`：只接收不早于最后已接收 readSequence 的列表；已知单项目只接受不降 revision 的版本；保留本标签刚创建但旧列表尚未包含的项目。
- `workspace.apply(project)` 合并单项目 mutation 结果；`select(projectId)`、`selected()`、`projects()`；选择代次 `selectionGeneration` 每次实际选择变化递增。
- `draft(projectId) -> {text, baseRevision, baseRequirement, dirty, conflict}`；`editDraft(projectId,text)` 仅第一处修改捕获基线；`saved(projectId, submittedText, response)` 仅提交内容匹配且未被新编辑替换时清草稿。
- `feedback(projectId, value?)` 按项目保存 `{level, kind, message, commandId?, attemptId?}`，连接状态在模型外独立；关闭时只删该条。
- 选中项只用 `relay-project`；draft 不写 storage；列表搜索不调用 select。记录未保存草稿时注册 beforeunload，清空后移除。

- [ ] **Step 1: 写并观察 Node RED。** `late_list_does_not_drop_new_project`、`lower_revision_cannot_rollback_mutation`、`older_read_cannot_replace_newer_read`、`selection_and_feedback_do_not_leak`；断言两个乱序列表后新建项目仍在且被选中。
- [ ] **Step 2: 写草稿 RED。** `blur_and_project_switch_preserve_draft`、`remote_revision_keeps_original_cas_baseline`：本地基线7、远端8，草稿文本不变且 expectedRevision 仍7；`save_response_does_not_clear_newer_text`。storage不可用仍能初始化/选择，不持久化需求正文。
- [ ] **Step 3: 实现纯数据模型并接入 live.js。** 不复制后台业务 transition；所有 DOM 投影从同一 selected state 取值。远端变化只标冲突，不自动合并正文；列表遗漏若不是已知旧读，标最后已知/需核实，不悄悄删用户项目或允许写入未知对象。
- [ ] **Step 4: 跑全部 Node 测试及现有 HTTP GREEN。** 选择、上传现有 revision/项目检查保留；未提交草稿不能被轮询 render 覆盖。

## Task 5: 一次提交与刷新后的只读核实

**Files:**
- Create: `live-operations.js`、`tests/live-operations.test.cjs`。
- Modify: `live.js`。

**Interfaces:**
- `RelayOperations.create({api, workspace, storage, now, uuid, changed}) -> operations`。
- `submit(kind, payload, {projectId, expectedRevision}) -> Promise<{status:'accepted'|'rejected'|'unknown'|'blocked', projectId, state?}>`；`createProject(name)` 同返回形状；`restore()` 只做 GET；`pending(projectId)`、`creationPending()`、`stop()`。
- storage key `relay-pending-v1`，描述 `{scope:'project'|'create', projectId:string|null, commandId, kind, expectedRevision:number|null, submittedAt:number}`；严格字段和类型检查，最多50项/32KiB；不含 name/需求/upload payload/token。
- payload-free持久化`relay-pending-guard-v1`，形状`{version:1,unresolved:true}`：每次POST前先写guard，再写descriptor；全部pending解决后先删descriptor、最后删guard。guard存在而descriptor损坏/超限/丢失时进入恢复阻塞，只读仍可用、所有项目写入及创建禁用，不允许忽略坏记录后继续提交。storage无法保存时仍保留内存阻塞并明确刷新保障不可用，不声称浏览器持久化可靠。
- 存储失败时 pending 留内存，明显提示“此标签可核实，刷新后无法恢复本次提交标识”；未知结果的阻塞不能因提示关闭而解除。
- 后台 receipt found 后，先读最新 state，再合并不低于 acceptedRevision 的对应项目；失败则继续未知。create receipt 指出项目后选择它；不重放 POST。404 receipt 表示旧后台不兼容，不等于未提交。
- 对同一项目未知结果拒绝其他写操作；全局 create 未知只阻止再次创建，允许查看已知项目。已确认的 rejected 才清 pending；found=false 永不自动清除。

- [ ] **Step 1: 写并观察 Node RED。** `response_lost_is_unknown_and_never_posts_twice`、`missing_receipt_keeps_lock`、`restore_only_reads`、`receipt_does_not_apply_stale_state`、`unsupported_receipts_stay_blocked`、`definitive_rejection_unlocks_without_replay`。断言两次 submit/刷新恢复后 POST 合计仍1；未知期间新 commandId 也不能穿透。
- [ ] **Step 2: 补边界 RED。** `corrupt_pending_with_guard_blocks_all_writes`与`guard_without_descriptor_does_not_unlock`：损坏/超量/丢失pending仍只能GET，submit/create均blocked且POST计数0；descriptor中伪造kind也不得执行；storage getter/读取/配额失败不丢内存阻塞；创建结果未知不重复新建，不把其反馈挂到错误项目。
- [ ] **Step 3: 实现操作状态机。** POST 前登记；成功写入 workspace 后核实/清除；网络、JSON、5xx或无明确 outcome 的错误保持 unknown。receipt按1200ms开始并有界退避，仅GET；不做“重试此请求”按钮。确定拒绝保留可读错误与草稿。
- [ ] **Step 4: 接入所有项目 mutation、创建和刷新恢复。** 先恢复pending再开放写按钮；上传仍先严格UTF-8/256KiB本地检查。生成前保存必须得到 accepted、同项目同内容/代次，才允许新的 generate；unknown/rejected/blocked 一律不能继续。
- [ ] **Step 5: 跑全部 Node 与现有后端/HTTP GREEN。** 对源码grep/只读审阅确认没有 setTimeout retry POST、没有恢复时调用 generate/start。

## Task 6: 明显的项目导航、真实项目身份与可恢复编辑

**Files:**
- Modify: `live.js`、`styles.css`、`tests/p1/browser.cjs`。
- Create: `tests/p1/stability-browser.cjs`、`tests/p1/run_stability_browser.py`。

**Interfaces:**
- 新 DOM：`#my-projects`、`#project-search`、`#project-list`、`#current-project-name`、`#requirement-dirty`、`#operation-feedback`、`#operation-dismiss`。
- 保留旧 `#project-select` 作为可见兼容下拉，列表与下拉共用同一选择函数，不各存一份状态；列表按钮 `data-project=<id>`、选中 `aria-current=true`。
- 顶部导航放在 prototype-notice 之后、setup 大面板之前，375和1440首屏可见；列表可折叠，顶部入口/当前名称始终可见。保留搜索焦点和同名短ID，短ID显示前8位但动作始终使用完整ID。列表行展示阶段及events中最后一个有效time（后台秒数转显示时间），无可靠time写“暂无活动记录”；离线旧列表明确标最后已知，不能编造创建/活动时间。
- sidebar/breadcrumb/project badge/document.title 动态投影当前名称，使用 textContent；没有项目时显示“尚未选择项目”，不是示例名。

- [ ] **Step 1: 写实际浏览器 RED。** 准备至少3个真实HTTP项目，其中两个同名，第三个名称包含 HTML 字符；断言首屏“我的项目”、短ID、真实活动time/无time文案、搜索空结果/无项目状态、键盘选择、轮询不丢焦点、搜索不改选中、刷新保留选择、无XSS。
- [ ] **Step 2: 写草稿/竞态 RED。** 失焦/切换回来内容仍在；另一标签HTTP修改使本地CAS失败后草稿和错误保留；上传File.arrayBuffer晚返回、请求期间切换按钮禁用，绑定检查仍拒绝过期结果。通过两个失败保存（明确409、未知响应）分别断言 generate POST为0。
- [ ] **Step 3: 实现导航和编辑UI。** 列表按ID更新文本/顺序，不每poll replaceChildren；成功保存清dirty，冲突给明确提示而非自动覆盖。切换即清iframe src及旧遥测投影。
- [ ] **Step 4: 运行新浏览器套件 GREEN。** `"$PY" -I -B -X utf8 "$DEV/tests/p1/run_stability_browser.py" --group navigation`；375/1440截图必须实际查看，无横向溢出/页面错误。保留现有browser.cjs覆盖，不删除旧断言来迁就新UI。

## Task 7: 首屏可信进度与按项目持续的操作反馈

**Files:**
- Modify: `live-workspace.js`、`live.js`、`live-output.js`、`styles.css`。
- Modify: `tests/live-workspace.test.cjs`、`tests/p1/stability-browser.cjs`。

**Interfaces:**
- `RelayWorkspace.describeProgress(project, telemetry|null) -> {title, detail, next, startedAt:number|null}`；UI `#current-progress`、`#progress-title`、`#progress-next`、`#last-sync`。
- `RelayOutput.mount({node,api,observed})` 新可选 `observed(page|null)`；只发已经通过项目/attempt/epoch绑定检查的摘要，换项目/断线立即null。page.authority.revision落后当前state或activeAttempt不匹配时不能覆盖细阶段。
- 权威 phase始终来自state；当前activeAttempt.status=queued显示排队，developer按run.done与开发任务统计X/Y；遥测仅补充 host_validation/trusted_rerun 和真实计时。
- 反馈只在新动作、明确关闭或对应attempt的权威结束时改变；连接poll不得改写。ready明确“实际测试和可信复跑通过，待人工验收”，accepted仅来自后台人工命令。

- [ ] **Step 1: 写并观察纯模型 RED。** 表驱动覆盖所有现有phase、pauseRequested、leaseUnknown/recoveryRequired优先、queued、host_validation、trusted_rerun；没有起点不编造elapsed，旧attempt/sealed/CLI exit0不能变ready，缺telemetry保留state。
- [ ] **Step 2: 写浏览器 RED。** 保存成功/上传失败/明确拒绝经过3轮成功poll仍存在；切换项目只显示其反馈；pending显示结果待确认而非“失败已取消”；离线显示最后同步时间/最后已知状态。下载只显示“已交给浏览器下载”，不能声称磁盘保存成功。
- [ ] **Step 3: 实现进度投影与反馈。** 首屏可见，无假百分比/ETA；编辑/生成/拆分/逐项批准/开始/暂停/停止/重试/下载都有前后状态与安全下一步。日志结束和RESULT不触发项目完成文案。
- [ ] **Step 4: 运行 Node 与 `run_stability_browser.py --group feedback`、现有HTTP/遥测 GREEN。** 保存并实际查看375/1440截图。

## Task 8: 真实HTTP故障验证、全套回归与独立交付

**Files:**
- Modify: `tests/p1/run_stability_browser.py`、`tests/p1/stability-browser.cjs`、`tests/p1/support.py`。
- Create: `tools/run_free_validation.py`（从版本化测试入口运行并保存结果，不调用模型）。
- Modify: `README.md`、`START-HERE.txt`、`USER-GUIDE.html`（仅DEV）。
- Create: 新输出目录下的测试报告、源/测试哈希、依赖清单、候选/验证/最终包记录。

**Interfaces:**
- 稳定性runner groups为 navigation/feedback/faults/soak/all，默认all；workers=False，固定Setup fixture明确标注；不生成真实模型结果。
- 故障门仅存在测试支持：真实Store/action_lock或HTTP传输边界用Event确定性阻塞/释放，无生产后门。
- `tools/run_free_validation.py --root <DEV> --output <new-dir>` 顺序跑FULL并输出每套件exit/treeStopped/timedOut/跳过原因；输出目录必须全新。验证副本的tests/tools准备只一次，杜绝旧helper并行copytree竞争。

- [ ] **Step 1: 写并观察faults RED（可回放旧基线证明回归有效）。** Playwright `route.fetch()`让真实POST提交后丢弃响应；另一个用真实action_lock保持POST等待而GET receipt found=false，再放锁；分别刷新页面，断言总项目数/commands/attempts增长最多一次。明确拒绝409、提交后500、receipt404、state/setup/telemetry挂起、403、旧列表/遥测乱序分别测试。慢setup时项目state仍更新。
- [ ] **Step 2: 实现测试支持并修复仅实际暴露的问题。** 所有故障门try/finally释放，测试超时后Job树停止；不通过缩短生产deadline或放松auth制造绿灯。若需测试deadline加速，测试时钟/已定义接口注入仅存在测试，不加生产HTTP配置开关。
- [ ] **Step 3: 跑有界soak。** 3个真实项目，30轮切换/只读刷新，每5轮一次人为网络中断恢复；每轮断言选择、草稿、状态不串、无额外mutation。记录请求最大并发、进程数与留存DOM，不以循环有限通过承诺绝对稳定。
- [ ] **Step 4: 跑FULL。** 任何失败/skip逐项记名；比较BASE保护文件仍匹配初始哈希，index/app/model/demo原字节不变。审阅新接口无正文泄露、未知操作无重放。
- [ ] **Step 5: 更新DEV内手册并构建新Windows包。** 新JS入包；候选、验证副本、最终包三者分开。固定上游依赖重用已验证输入，旧r3不改；最终ZIP独立解压逐项哈希、中文空格短路径实际CMD启动/重开/退出、包内FULL串行复测。新增报告按用户流程列结果，不只列总数。
- [ ] **Step 6: 到交付关口再执行现有一次真实计数器授权。** 只用最终新包；条款字节相同才沿用已获决定并经正常UI记录，否则重新征得同意。真实设计→审阅上传→拆分→逐项批准→手动开始→开发→独立测试→可信复跑至ready；不自动paid retry、不代accepted。保留耗时、费用CLI报告与失败证据。
- [ ] **Step 7: 分开报告剩余门槛。** 实际干净Windows环境不可用就标未验证并说明所需环境；最终验收交给用户。若需要停止/备份/更新58593，单独征得许可；在此之前只交独立新包和新目录入口。

## 覆盖与自查（计划阶段，不是执行结果）

- Spec 1–3/6：Task1隔离、Task2兼容、Task8安全交付；不动在线实例。
- Spec 4.1/5.3：Task4/6导航、草稿、CAS、异步绑定。
- Spec 4.2/4.3：Task5/7持续反馈、首屏进度和下一步。
- Spec 5.1/5.2：Task2/3/5/8有界读取、回执、未知提交、幂等创建与故障注入。
- Spec 7：Task8全套及有界循环；Task2旧DB/auth；Task6/7桌面与375截图。
- Spec 8/9：Task8保留真实模型、干净环境和人工验收的独立门槛；无范围外自动化。
- Spec 10：由同目录 `2026-10-24-ubuntu-cross-platform.md` 实施，必须沿用本计划的相同业务与UI，不复制代码。
- 每一Review Focus已落入对应任务的RED；接口名/回执形状和后续消费者一致；命令使用仓库现有unittest/Node/真实browser路径，无pytest或新增框架假设。

## 两份计划的执行顺序与付费关口

1. 先完成本计划Task1–7及Task8的免费故障/源码/Windows候选验证；Task8 Step6暂不消耗真实模型授权。
2. 再完成Ubuntu L1–L6可实施部分。其修改共享server/environment/执行接口后，必须重新跑本计划FULL并重新构建、复测Windows最终包，不能用平台改造之前的Windows证据为后来字节背书。
3. Linux环境或资产阻塞时，不影响对已经验证的Windows版本单独报告；明确冻结该Windows交付版本，再在该最终包上执行一次已授权真实闭环。之后的Linux工作使用另一个独立源码副本；任何后续Windows新包都不得继承旧包真实模型通过标记，额外付费测试另获授权。
4. Ubuntu L7和本计划Task8剩余验收各按实际环境/权限完成，ready与用户accepted始终分开。

## 执行交接

用户先审阅本计划及Ubuntu计划，再选择 Native 或 Subagent-driven。没有选择前不创建开发副本、不写产品代码。本计划建议 Native 顺序执行，每任务保存RED/GREEN和文件checkpoint，整体验证后独立审查；这种方式减少跨任务接口在多次交接中丢失，也避免对这些强耦合安全改动并行写入。
