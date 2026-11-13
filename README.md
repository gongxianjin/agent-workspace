# AgentRanch

**把"定稿设计文档"变成"已验收交付物"的本地工作台**：原生 Rust + egui 的 Windows 管理器 + Linux agent-worker。设计文档导入 → 任务拆分 → 逐项审核 → 手动开发 → 独立测试 → 人工验收，每一步都有显式门槛和可追溯证据，绝不把"看起来完成了"当作"完成了"。

```
┌────────────────────── Windows ──────────────────────┐   ┌──── Linux B ────┐
│  agentranch.exe (GUI Manager)                        │   │  aw-cli (worker) │
│  ├─ 项目工作流（导入→拆分→审核→开发→测试→验收）      │──▶│  签名派工/回传    │
│  └─ aw-cli.exe (CLI / 受管 worker，同目录同版)       │   │  退出证据        │
│  SQLite 账本 · 阶段门控 · 操作回执                    │   └─────────────────┘
└──────────────────────────────────────────────────────┘
```

## 30 秒开始

1. 从 [Releases](https://github.com/gongxianjin/agentranch/releases) 下载 zip，解压到任意目录（**不要**装进 Program Files）。
2. 双击 `agentranch.exe`（首次会有 SmartScreen 提示，见下方 FAQ——包未签名是预期）。
3. 左侧输入项目名 → 新建项目 → 导入一份**已定稿**的 UTF-8 `.md`/`.txt` 设计文档 → 核对 SHA-256 → 确认版本 → 按界面引导走完流程。

CLI 入口：`aw-cli.exe --help`（`check` / `mcp` / `local-check` / `runner` / `controller`）。

> ⚠️ 当前版本边界（诚实声明）：拆分/开发/测试需要连接真实执行主机；未配置时按钮会以人话告诉你差什么、怎么补。外部 AO/Reason/模型链路、远程退出证据仍在建设中，详见[当前明确未覆盖](#当前明确未覆盖)。

## 为什么是 AgentRanch

- **证据优先**：每个动作落 SQLite 账本；"已接受请求"≠"已执行"，"测试通过"≠"验收通过"，阶段与证据一一对应。
- **显式门控**：全部 14 个阶段（①新建 → ✓已验收）都有人工确认点；驳回必须写原因，返修最多 2 轮，验收必须亲自点。
- **本地优先**：状态、账本、产物全在本地 `--state-dir`；不匿名上报，不云依赖。
- **一处入口**：GUI 看进度与证据，CLI 做脚本化/MCP 集成，二者共享同一账本。

## 快速导航

| 想做什么 | 去哪里 |
|---|---|
| 图形界面走完整流程 | [USER-GUIDE.md](USER-GUIDE.md) §快速上手 |
| CLI/脚本/MCP 集成 | [USER-GUIDE.md](USER-GUIDE.md) §CLI 参考 |
| 配置 Linux Worker | [USER-GUIDE.md](USER-GUIDE.md) §Linux Worker 配置 |
| 查看每个阶段的意思 | GUI 项目页"当前阶段"行（①–✓ 编号即顺序） |
| 理解"被阻塞"怎么办 | 界面 ⚠ 卡片的【现在的情况】/【怎么办】三段式说明 |
| 验证与证据口径 | 本 README [打包与验收边界](#打包与验收边界) |
| 从源码构建/回归 | 本 README [开发构建](#windows-开发构建使用已安装-sqlite) |

## GUI 启动方式

在解压后的产品目录中打开 PowerShell 或命令提示符：

```powershell
.\agentranch.exe                                  # local 模式（本地诊断）
.\agentranch.exe --real                           # real 模式（需配置执行依赖）
.\agentranch.exe --state-dir "D:\state\workbench" # 显式状态目录
```

- `--state-dir` 必须后跟非空路径，不能重复；未知参数会显示启动错误而不是静默忽略。
- 默认状态目录：local `%LOCALAPPDATA%\agentranch\local`，real `%LOCALAPPDATA%\agentranch\real`。两种模式账本身份不同，不要互换或删除账本绕过错误。
- 两个 exe（`agentranch.exe` + `aw-cli.exe`）必须同目录同版本；GUI 启动时会校验。

## CLI 子命令

```powershell
.\aw-cli.exe --help
.\aw-cli.exe check --config .\config.json
.\aw-cli.exe mcp --config .\config.json
.\aw-cli.exe local-check --state-dir "$env:LOCALAPPDATA\agentranch\local"
.\aw-cli.exe runner --config .\config.json --once
.\aw-cli.exe controller --config .\config.json --once
```

- `check` 只校验配置（`external_readiness: not_probed`），不连接外部服务。
- `local-check` 运行本地固定诊断，输出带 `simulated: true`，不是远程执行证明。
- `mcp` 走 stdin/stdout 协议；不要把日志混入协议流。
- `runner`/`controller` 需要受信任配置；`--once` 与 `--stop-on-stdin` 互斥。
- 非零退出码时保留 stdout/stderr 与状态目录；不要靠重跑或删账本把 `UNKNOWN` 当成功。

## local 与 real 的区别

- **local**：随 Rust 核心内置的受控 fixture，用于熟悉界面/检查状态目录/SQLite/受管进程，明确标记模拟，不代表真实执行。
- **real**：显式真实模式。配置缺失不会静默回退 local；需要受信任配置提供 AO/Reason/节点依赖，由实际运行与退出证据证明链路可用。仅配置文件存在、`check` 通过、按钮可点都不等于真实执行完成。

## 常见问题（FAQ）

**双击后提示找不到 worker** — 确认 `aw-cli.exe` 与 `agentranch.exe` 同目录且来自同一冻结包；不要从 PATH 拼接。

**SmartScreen/签名警告** — 包默认未签名，属预期；本包无 installer、无签名声明。

**状态模式不匹配** — 复用了另一模式的状态目录。保留原目录作证据，用正确的 `--state-dir` 重启；不要删文件强启。

**real 模式未就绪** — 诚实失败路径。按界面 ⚠ 卡片的【怎么办】步骤补配置；以实际探测与退出证据为准。

**"拆分任务"点了没反应？** — 先看按钮下 ⚠ 卡片：未配置执行主机会明确告知三步操作；已配置但链路未打通会说明这是版本能力边界，不是操作问题。

## Windows 开发构建：使用已安装 SQLite

不启用 `rusqlite/bundled`、不编译 `sqlite3.c`；使用本机已安装的 x64 MinGW SQLite（当前环境 `C:\msys64\mingw64`，SQLite 3.52.0；最低 3.45.3）：

```powershell
.\packaging\cargo-windows.cmd test --target x86_64-pc-windows-gnu --locked --offline --no-default-features
.\packaging\cargo-windows.cmd build --target x86_64-pc-windows-gnu --locked --offline --bins
```

入口只为子进程设置 MinGW-first PATH、固定 Rust 1.99.1、`SQLITE3_INCLUDE_DIR`/`SQLITE3_LIB_DIR`、`SQLITE3_STATIC=0`、`SQLITE3_NO_PKG_CONFIG=1`，不改全局环境、不下载安装。Windows 包必须经 PE import 递归收集器携带 SQLite DLL 及实际依赖；打包成功不等于运行验收通过。其他平台需本地可链接 SQLite；不声明 Linux 构建已验证。

一键回归（Rust 388 + Python 119，约 3–4 分钟）：

```powershell
py -3 -I .test-scripts\local\run_regression.py
```

## 打包与验收边界

`packaging/package_windows.py` 只从已生成且同版身份绑定的输入构造可审计便携目录和 ZIP；不构建 Rust、不启动 GUI、不外发请求、不签名、不安装。

冻结版要求同一 source inventory、offline Cargo metadata、build identity 与同目录两个 Windows PE；build identity 机械校验 source digest、target、toolchain、profile、版本及两个 exe 的字节 SHA-256。打包前检查：两 exe 存在且为 PE、source 无漂移、build identity 一致、PE imports 可解析、输出已存在则失败保留旧证据、`PACKAGE-MANIFEST.json` 与 ZIP entry/字节回环核对。失败退出码 `2` 并保留 `PACKAGE-FAILURE.json`；成功状态 `PACKAGE_CREATED_RUNTIME_NOT_YET_VERIFIED` 仅表示包内输入与哈希核验完成，发布前仍需同版原生运行验收。

## 当前明确未覆盖

以下边界标为 `NOT_RUN`：外部 AO/Reason/模型连接与真实执行、真实项目开发、远程退出/取消、Windows native window acceptance、签名/installer、Stop10 及其他未实际运行的 gate。已运行验证限于本地 headless Rust/SQLite、真实 GUI pointer action、本地 service/ledger、synthetic ingress、独立 `ExitProof` 与 managed-artifact 状态投影；这些结果不证明外部执行或真实产品包已验收。

---

*AgentRanch · native Windows manager + Linux agent-worker workbench · Rust + egui + SQLite*
