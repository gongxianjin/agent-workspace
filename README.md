# AO + Reason Workbench for Windows

这是一个原生 Rust + egui Windows 工作台；当前源码定义两个 Windows 二进制入口，分别用于 GUI 和 CLI/受管 worker。它们不是旧版 Python/Web 入口的别名，也不会自动安装服务、修改 PATH 或迁移旧账本。

## 先看懂源码定义的两个 Windows 二进制入口

| 源码入口 | 用途 | 形成同版 Windows 构建并通过验收后的使用方式 |
| --- | --- | --- |
| `ao-reason-workbench`（GUI 二进制） | 管理应用和项目工作流的 GUI；源码入口用于在窗口中查看状态、项目阶段、阻塞原因和操作回执。 | 只有同版 Windows 构建与配套 worker 通过运行验收后，才可双击对应的 `ao-reason-workbench.exe`；也可从终端传入下方 GUI 参数 |
| `aw-cli`（CLI/受管 worker 二进制） | 无界面 CLI/受管 worker 入口，适合脚本、诊断、MCP/stdin 场景；它不是 GUI 的替代启动器。 | 只有同版 Windows 构建通过验收后，才可使用对应的 `aw-cli.exe` 并显式提供子命令 |

只有在同版 Windows 构建、配对和运行验收完成后，两个对应的 exe 才构成可交付产品入口。届时它们必须保持在同一目录；GUI 启动时会按自身目录查找同版 `aw-cli.exe`，缺失或版本不一致时不要用其他目录的 exe 代替。

## GUI：真正支持的启动方式

在解压后的产品目录中打开 PowerShell 或命令提示符：

```powershell
.\ao-reason-workbench.exe
.\ao-reason-workbench.exe --real
.\ao-reason-workbench.exe --state-dir "D:\ao-state\workbench"
.\ao-reason-workbench.exe --real --state-dir "D:\ao-state\workbench-real"
```

不带 `--real` 时是 `local` 模式；带 `--real` 时是 `real` 模式。`--state-dir` 必须后跟非空路径，不能重复；未知参数会让 GUI 显示启动错误，而不是静默忽略。

Windows 默认状态根目录来自 `%LOCALAPPDATA%`，具体目录为：

- local：`%LOCALAPPDATA%\ao-reason-workbench\local`
- real：`%LOCALAPPDATA%\ao-reason-workbench\real`

显式 `--state-dir` 会覆盖默认目录。local 和 real 使用不同的状态身份；不要把一个模式的账本复用给另一个模式，也不要为了绕过错误删除账本。状态目录包含 SQLite/锁/身份文件，建议先备份再处理故障。

GUI 中的“本地诊断”只打开本地 Rust 固定 fixture。它明确标记为模拟 AO/Reason/模型，不代表外部服务已连接。

## CLI：真正支持的子命令

先查看内置帮助：

```powershell
.\aw-cli.exe --help
```

当前源码提供这些入口：

```powershell
.\aw-cli.exe check --config .\config.json
.\aw-cli.exe mcp --config .\config.json
.\aw-cli.exe local-check --state-dir "$env:LOCALAPPDATA\ao-reason-workbench\local"
.\aw-cli.exe runner --config .\config.json --once
.\aw-cli.exe runner --config .\config.json --stop-on-stdin
.\aw-cli.exe controller --config .\config.json --once
```

- `check` 只校验配置并输出 `external_readiness: not_probed`；它不会连接外部 AO/Reason。
- `local-check` 运行本地固定诊断，输出中会标明 `simulated: true`；它不是远程执行证明。
- `mcp` 使用 stdin/stdout 协议；不要把普通日志或交互文本混入协议流。
- `runner`、`controller` 需要受信任配置。`--once` 与 `--stop-on-stdin` 不能同时使用。
- real 模式不会自动回退到 local fixture。配置存在不等于外部执行链已就绪。

CLI 返回非零退出码时应保留 stdout/stderr 和状态目录，不要通过重跑或删除账本把 `UNKNOWN` 当成成功。

## 新手操作顺序与当前阶段限制

以下是界面中的显式阶段顺序，不表示整条产品链已实现或验收。当前已由本地 headless Rust/SQLite 回归覆盖的范围包括项目创建/导入/确认、任务审核与阶段门槛，以及通过真实 GUI pointer action 触发的 Test、Repair、Accept、Redo 请求和其持久化门控；测试使用 synthetic ingress/fixture 数据，验证的是本地请求、状态、依赖闭包、ExitProof 与 managed-artifact 状态边界。外部 AO/Reason/模型执行、真实项目开发、远程退出/取消、Windows native window 和已验收产品包仍未建立，不会用固定样例伪装完成。`Start/Test/Repair/Accept` 作为门控入口可见，不等于现在能完成全流程。

1. 双击 `ao-reason-workbench.exe`，确认两个 exe 在同一目录。
2. 创建项目。
3. 导入已经定稿的 UTF-8 `.md` 或 `.txt` 设计文档。文件上限为 256 KiB；导入不会重新生成设计，也不会自动调用模型。
4. 检查显示的上传版本 `U`、文件名、完整文本和小写 SHA-256。只有在确认内容确实是要执行的文档后，点击“确认此上传版本”。文档版本或哈希过期时，确认会被拒绝。
5. 按已确认的上传版拆分任务。
6. 逐项检查标题、描述、验收标准、依赖和公开输出。每项都要人工点击“审核通过”；驳回必须提供原因，可对驳回项单独“仅重做”。
7. 所有任务审核通过后，点击“手动开始开发”。解锁按钮不等于开发已完成，也不会自动开始。
8. 开发阶段检查会话、阶段、公开输出和阻塞原因。
9. 点击“运行独立测试”。`Test` 只表示独立测试动作或结果，不等于人工验收。
10. 测试失败时填写明确返修原因，点击“提交返修”，完成返修后再次测试。
11. 只有在测试通过且你检查了证据后，才点击“人工验收通过”。`Accept` 是明确的人工作业，不会因任务审核通过或 Test 按钮被点击而自动发生。

对于审批型任务，界面会展示待审批请求和可用决定。决定按钮只提交受范围、会话、attempt、generation 和有效期约束的请求；“已接受请求”或 `queued` 不等于远端已经执行。

## local 与 real 的区别

### local

local 是随 Rust 核心提供的受控本地 fixture，用于检查状态目录、SQLite、受管子进程、固定模板和界面回执。它可以帮助新手熟悉界面，但不代表真实 AO、Reason 或模型运行。

### real

real 是显式选择的真实模式。当前产品不会因为 real 配置缺失而静默使用 local。需要由受信任配置提供 AO/Reason/节点等依赖，并由独立的实际运行、退出证据和产品验收证明链路可用。仅有配置文件、`check` 输出、ready/probe、GUI 状态或成功提交按钮都不足以声称真实执行完成。

本版本文档不声称已经验证外部 AO/Reason 连接、真实模型调用、跨机网络、远程进程退出、Windows 原生窗口验收或 Linux 构建。

## 操作语义和安全边界

- “查看状态”“查看报告”“刷新审批”是读取或请求状态；不替代独立证据。
- 关闭 GUI 会停止本地工作台并等待本地后台收尾，不代表远端任务已取消。
- 不要手工修改 SQLite、身份文件、上传文档哈希或审批字段来绕过阶段门槛。
- 不要把历史报告、旧 attempt、旧 session 或旧审批决定当作当前执行证明。
- 未配置、未实现、执行失败和证据尚未建立是不同状态；按界面给出的阻塞原因处理。

## 常见错误

### 双击后窗口提示找不到 worker

确认 `aw-cli.exe` 位于 `ao-reason-workbench.exe` 同一目录，并确认两者来自同一个冻结包。不要从 PATH 或其他构建目录拼接 exe。

### 状态模式不匹配

你可能复用了另一个模式或部署的状态目录。关闭工作台，保留原目录作为证据，使用正确的 `--state-dir`；不要删除文件强行启动。

### real 模式未就绪

这是预期的诚实失败路径：real 不会自动回退 local。检查受信任配置、节点准入/健康、AO 项目和 Reason 依赖，并以实际探测与退出证据为准。

### Windows SmartScreen 或签名提示

产品包默认未签名。SmartScreen 警告不是产品功能失败，也不代表产品已签名；本包不包含 installer 或签名声明。

## Windows 开发构建：使用已安装 SQLite

不再启用 `rusqlite/bundled`，也不编译 `sqlite3.c`。使用本机已安装的 x64 MinGW SQLite（当前环境 `C:\msys64\mingw64`，SQLite 3.52.0；最低 3.45.3），而不是 `sqlite3.exe` 命令行工具替代链接库：

```powershell
.\packaging\cargo-windows.cmd test --target x86_64-pc-windows-gnu --locked --offline --no-default-features
.\packaging\cargo-windows.cmd build --target x86_64-pc-windows-gnu --locked --offline --bins
```

入口只为子进程设置 MinGW-first PATH、固定 Rust 1.99.1、`SQLITE3_INCLUDE_DIR` / `SQLITE3_LIB_DIR`、`SQLITE3_STATIC=0` 和 `SQLITE3_NO_PKG_CONFIG=1`，使用现成的 `libsqlite3.dll.a` 与 `libsqlite3-0.dll`，不修改全局环境、不下载或安装。可先设置 `MINGW_HOME` 指向同布局的已安装环境。Windows 包必须通过现有 PE import 递归收集器携带 SQLite DLL 及其实际依赖；打包成功不等于已通过搬迁运行验收。其他平台需要本地可链接的 SQLite；此处不声明 Linux 已验证。

## 打包与验收边界

`packaging/package_windows.py` 只负责从已经生成并经过同版身份绑定的输入构造可审计便携目录和 ZIP。它不会构建 Rust、启动 GUI、发起外部请求、签名或安装产品。

正式冻结版必须使用同一 source inventory、offline Cargo metadata、build identity 和同一目录中的两个 Windows PE。build identity 机械校验 source inventory digest、target、toolchain、profile、版本及两个 exe 的字节 SHA-256；它不是独立的编译证明，也不是 native/window/GUI acceptance。

打包前会检查：

- `ao-reason-workbench.exe`、`aw-cli.exe` 均存在且是 PE（`MZ`）；
- source inventory、`Cargo.toml`、`Cargo.lock`、README 和相关 source identity 没有漂移；
- build identity 与 source digest、两个 exe 实际哈希一致；
- PE imports 可由 Windows API set、系统 DLL 或明确 runtime 目录解析；
- 输出目标已存在时不覆盖，而是失败并保留旧证据；
- 生成 `PACKAGE-MANIFEST.json` 中的 payload 字节数和 SHA-256，并写入 ZIP；packaging-only 回归会独立读取 manifest、目录和 ZIP，核对 entry 集合与实际字节。

失败时脚本退出码为 `2`，并尽可能在已创建的包目录保留 `PACKAGE-FAILURE.json`。成功状态 `PACKAGE_CREATED_RUNTIME_NOT_YET_VERIFIED` 只表示包装输入和文件哈希检查完成；发布前仍需要同版 Windows 原生运行、双击/CLI 验证、资源门禁和独立验收。

## 当前明确未覆盖

本 README 明确将以下边界标为 `NOT_RUN`：外部 AO/Reason/模型连接与真实执行、真实项目开发、远程退出/取消、Windows native window acceptance、同版双 EXE 产品包运行验收、签名/installer、Linux 构建、Stop10 及其他未实际运行的 gate。已运行的验证限于本地 headless Rust/SQLite、真实 GUI pointer action、本地 service/ledger、synthetic ingress、独立 `ExitProof` 和 managed-artifact 状态投影；这些结果不证明外部执行或真实产品包已验收。
