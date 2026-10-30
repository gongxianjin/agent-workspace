# 接力 · 三 Claude 本机协作工作台（Linux 版）

## 1. 需求

在一个网页里完成：

**填写需求 → AI生成设计 → 下载人工修改 → 上传确认版 → 按上传版拆分 → 逐项审核／驳回局部重做 → 全部通过后手动开工 → 开发 → 独立测试／显式返修复测 → 人工验收。**

HTTP真实模式使用三个独立原生Claude Code进程，分别负责架构、开发、测试；reason-network传递TASK → 模型显式ACK → RESULT。阶段串行、共享受管理项目目录，不是本聊天子Agent，不要求三个长期终端，不使用Go、Electron或worktree。

只支持本服务新建的受管理项目，产品限无需安装依赖或构建的静态HTML/CSS/JS。网页不能指定任意已有业务目录、Shell、可执行文件或argv。

**交付定位：本分支（v0.1-linux）是原 Windows x64 功能预览版的 Linux x64 适配版，不是正式稳定版。** 启动、单实例锁、进程上下文与原生资产校验已按 Linux 重写：systemd 临时单元承载后台、`fcntl` 排他锁、`native-runtime.json` 清单逐文件 SHA256 校验；浏览器不再从官方源下载 Windows 组件，由本机固定资产（chrome-linux64）提供。包内随附的 USER-GUIDE.html 与 START-HERE.txt 仍为 Windows 版原文，通用流程可参考，涉及 Windows 专属操作以本 README 为准。

## 2. 行动

### 2.1 第一次使用，按这里操作

1. 系统要求：Linux x64、Python 3.12（`python3.12` 可执行）、systemd（`systemd-run`／`systemctl` 可用）。root 以 system 单元运行，普通用户以 user 单元运行。无需自备 Node／Playwright／浏览器：`native-assets/` 内提供受校验的 claude、node、python3.12、reason-network 与 chrome-linux64。
2. 在工作台目录执行 `./start-workbench.sh start`：创建或复用同一受管 systemd 临时单元（单元名 `relay-console-<哈希>`，与 UID＋数据根绑定），控制台与预览默认监听回环地址的 `8787`／`8788` 两个端口，可自定义。重复 start 只复用同一实例，不重启任务。
3. 执行 `./start-workbench.sh access`：在 `userdata/private-access/<ID>/launch.txt` 写入带一次性认证的页面 URL，60 秒内用本机浏览器打开。不要把裸地址复制到另一个浏览器或新会话：没有启动器的一次性认证信息，页面能显示不等于已连接；会话失效时重新 access，不用重启后台。
4. 自己阅读并决定是否同意页面显示的当前条件。确认不会自动检测、下载或调用模型。
5. 按官方安装文档自行登录原生 Claude Code（`claude auth login`）。工作台不填密钥、不代登录、不复制发送者账号；native-assets 中的 claude 可执行文件受清单校验，账号仍归使用者。
6. Linux 版无浏览器下载步骤：测试浏览器来自本机固定资产清单，逐文件校验通过即就绪；不会下载 Windows 组件，也不会自动接受任何发行条款。
7. 状态允许后开始自己的小型非敏感项目。生成、拆分、开发、测试、返修、显式重试均可能计费。
8. 等所有项目停稳，点网页"退出工作台"，或执行 `./start-workbench.sh stop`（仍有活动／未知租约时会被拒绝）。关闭浏览器标签或终端不退出后台 systemd 单元。

### 2.2 检测通过不等于模型已接通

Linux 执行门禁 = 清单资产校验（native-runtime.json 逐文件 SHA256）＋ 受管 systemd 宿主上下文激活 ＋ 本地依赖健康检查，三者齐备才允许执行；任一缺失即 fail-closed，模型操作保持禁用。`modelConnection` 恒为 `not_verified`，只有真实任务能验证接入；root 运行时浏览器执行受额外限制（`root_browser_execution`），不会关闭沙箱迁就。固定 version／help／auth status 检测不调用模型；登录未知时，可自行确认"使用已有本机配置继续（接入未验证）"，下次重检撤销此确认。刷新只读缓存，不替换正在运行任务的客户端快照。

### 2.3 "生成设计文档"按钮不可用时

按钮下方显示当前阻塞原因和下一步，不需要猜测是否点击成功。请先新建或选择受管理项目；接入未检测时，先在页面上方点击"重新检测（不调用模型）"。登录未知且必需参数及本地依赖通过时，可自行选择"使用已有本机配置继续（接入未验证）"；工作台不自动确认，也不自动付费探测。

会话失效时重新执行 `./start-workbench.sh access` 获取新的认证 URL，不用重启后台。任务／上传进行中或旧租约未知时仍会锁定，提示不取消这些保护。按钮启用只表示允许提交，不代表模型已成功执行。

### 2.4 审核、运行与实时输出

- 先生成设计，下载人工修改，再上传严格UTF-8的.md/.txt（≤256KiB），核对U版本与摘要。只有上传版可作为拆分依据。
- 每项单独通过或填写原因驳回；局部重做保留无关内容、版本和批准，受影响下游显式重审。
- 全部通过只解锁手动开始，不自动开工。开发、独立测试与宿主可信复跑串行执行。
- 首次实际浏览器检查返回测试员前冻结测试规格；失败由使用者显式返修，最多两轮，再按冻结规格复测。实际测试通过仍等待人工验收。
- "Claude实时输出"显示实际岗位、attempt、PID、公开assistant文本及宿主工具起止；不显示思维链、原始工具参数、system/user或stderr。
- 排队／准备与CLI计时分开；超时剩余来自实际wait deadline，不是预计完成时间。30秒静默不自动判失败；没有公开文本不伪造进度。
- 向上阅读暂停跟随，点"跟随最新"恢复；只保留有界尾部。旧attempt不会因刷新热转流式，不为日志重启当前实例。

发送成功≠模型已读，ACK≠完成，RESULT或exit0≠测试通过，实测通过≠人工验收。日志没有派工或完成权限。

### 2.5 数据、故障与交接

数据默认在同目录userdata（umask 077，属主私有），产物在 `userdata/projects/<项目ID>/product/`，浏览器检查证据在 `userdata/evidence/<检查ID>/`。原始evidence可能含需求、源码、工具正文及私密数据；公开输出脱敏不等于整个运行目录可以公开。原 Windows 版的 UTF-16 路径长度限制（path_too_long）在本版不适用，但仍拒绝符号链接的数据根与浏览器路径。

单实例保护为 `fcntl` 排他锁＋systemd 单元归属：已有属主存活时新启动只重开页面、不抢所有权。移动／备份前须通过网页正常退出，完整保留数据及SQLite辅助文件。不要旧、新目录同时处理同一数据。跨电脑、跨版本迁移和异常租约自动恢复未验收；不要删锁、手改数据库或提权强开。

遇到失败，先看手册故障表；求助只发版本、系统、步骤、固定stage／category／errno，不发私密fragment URL、launch.txt、认证或整个userdata。暂停调度不是冻结CLI；退出检查所有项目，排队、活动、暂停、停止中、未知租约或恢复待核验会被拒绝。

给别人只发原始干净分发包与手册，不压缩自己使用后的运行目录。不自动更新，不混用native-assets／清单／userdata。

### 2.6 源码与测试入口（不是普通使用者安装步骤）

源码模式依赖本机已有Python 3.12／Node／Playwright及同级reason-network；仅复制本目录不能代替完整分发包。直接打开index.html是明确标注的file://历史模拟，不调用真实AI；demo是预制资源。HTTP失败不降级模拟。

免费开发回归：

```bash
python -I -X utf8 -m unittest discover -s tests/p1 -p 'test_*.py' -v
python -I -X utf8 tests/p1/run_browser.py
python -I -X utf8 tests/p1/run_linux_browser_free.py
# 以下只限源码：真实本机固定非模型检测／接入确认；不提交生成、不接受发行条款。
python -I -X utf8 tests/p1/run_onboarding_browser.py
node --test tests/model.test.cjs
python -I -X utf8 tests/run_static_browser.py
```

这些测试只在开发／验证副本提供，不随干净用户包。模型与收费边界是明确fixture，本机进程、HTTP与Chromium真实执行。`live_probe.py`／`run_live_e2e.py`会真实计费，不能自动补跑。

## 3. 结果与边界

**本 Linux 适配分支的边界（诚实声明）：**

- 模型连接未验证：`modelConnection` 为 `not_verified`，最新原生Claude stream-json实际付费兼容未验收，synthetic协议fixtures不能代替。
- `linux_supervisor.py` 为 systemd 249 cgroup 原型（主PID存续期间纳管后代），当前生产控制台未接线；进程树整停仍依赖单元 KillMode=control-group 与既有停止证明。
- root 下浏览器执行受限（`root_browser_execution`），不关闭沙箱迁就。
- 干净Linux发行版／不同systemd版本／普通用户user单元的完整矩阵未验收。
- 完整三Claude真实闭环复验、内置工具拒绝对抗、全部自然语言AC、异常恢复均未验收。
- 本轮不开展许可证完整性审阅，保留已有原始通知，不提供法律再分发结论或公开发布批准。

cwd、提示词、串行租约、受控MCP和systemd cgroup不是完整OS文件系统／网络沙箱，不对敏感业务或强不可信代码作隔离承诺。

**原 Windows x64 版（v0.1）历史验证记录，不替代本分支核验：**

2026-10-23 Windows交付修复：浏览器证据短路径／UTF-16写前检查／原始错误保留，以及并发启动空锁文件初始化竞争（`RootLock` 写入刷新时的 `PermissionError(errno=13)`，修复后100组双进程实际启动均只有一个拥有者）。完整源码免费回归171/171（176.771秒）、HTTP真实Chromium17/17、状态模型31/31、历史静态Chromium17/17；遥测响应断连重复写500的问题已用真实TCP reset行为测试先复现FAIL再修复（HTTP专项24/24）。本机新短目录的官方下载完整链路已通过（181196793字节、固定SHA256、安全解压、真实浏览器健康、verifiedReceipt=true，317.409秒）。历史失败保留：两次TLS握手失败未归因；269字符深目标解压失败（同归档148字符目标成功）；后端159项crashbeat测试同步失败后仅修非空心跳前置条件（专项11/11）；HTTP初轮连接失败及并发启动exit1。曾有一次整体独立审查（Critical0、Important0、3个Minor延期）。

完整历史报告、原失败与裁决仅在源码开发目录dist/p1、dist/p2、dist/p3保留，不随用户包。
