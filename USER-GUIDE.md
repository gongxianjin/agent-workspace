# agentranch 用户手册（v0.2-win）

> 本手册与 v0.2-win 交付物对齐：Rust + egui 原生 Windows 管理端、随包 Python/Paramiko 网关、Linux 原生 Worker。旧版 Python/Web 入口已随本分支移除。
>
> **诚实边界**：本手册只描述已验证的能力。所有"已验证"均指本仓库 evidence 链背书的具体场景（详见附录 A），不泛化到未运行的环境。

## 1. 产品是什么

一个"Windows 管理 → Linux 真实执行"的跨机工作台：

```
Windows Manager（原生 GUI）          Linux Worker（172.28.8.221，示例部署）
┌─────────────────────────┐  SSH(Paramiko)  ┌──────────────────────────┐
│ agentranch.exe │ ──────────────> │ 原生 Rust ELF（owner-exec）│
│ + aw-cli.exe（同版配对）  │   signed start  │ → 真实模型调用 → artifact   │
│ + 随包 Python 运行时      │ <────────────── │ → signed terminal         │
└─────────────────────────┘  Store admission └──────────────────────────┘
```

- **项目工作流**（项目创建 → 设计文档导入确认 → 任务拆分审核 → 手动开发 → 测试/返修/验收）在 GUI 内闭环；
- **真实执行链**：GUI dispatch → Paramiko 网关 → Linux 原生 Worker → 进程内模型调用（TLS 证书锁定）→ artifact 回传 → Store admission → assignment 自动释放。

## 2. 包内容（v0.2-win 冻结包）

冻结包目录名形如 `agentranch-0.1.0-windows-x86_64-release-<source-sha8>`，包含：

| 文件 | 作用 |
|---|---|
| `agentranch.exe` | GUI 二进制（双击启动） |
| `aw-cli.exe` | CLI/受管 worker 二进制（**必须与 GUI 同目录同版**） |
| `runtime/python/` | 随包 Python 3.14.3 + Paramiko 4.0.0（网关用，不装服务、不改 PATH） |
| `libsqlite3-0.dll` | SQLite 3.52.0 运行库（包内携带） |
| `BUILD-IDENTITY.json` | source sha / 双 exe sha256 / toolchain 绑定 |
| `PACKAGE-MANIFEST.json` | payload 字节数与 SHA-256、ZIP entry 核对 |
| `README.md` / `PACKAGE-NOTES.txt` | 包内说明 |
| `source-identity/`、`SOURCE-INVENTORY.json`、`PYTHON-RUNTIME-INVENTORY.json`、`third-party-licenses/` | 审计身份与许可证 |

## 3. 安装与首次启动

1. 解压 ZIP 到任意本地目录（无需安装器；SmartScreen 警告见 §9）。
2. 保持 `agentranch.exe` 与 `aw-cli.exe` 在同一目录，不要用其他目录的 exe 拼接。
3. 双击 `agentranch.exe`（local 模式，受控 fixture），或在终端：

```powershell
.\agentranch.exe                        # local 模式
.\agentranch.exe --real                 # real 模式
.\agentranch.exe --real --state-dir "D:\ao-state\real-221"
```

- `--state-dir` 必须后跟非空路径，不能重复；未知参数显示启动错误而非静默忽略。
- 默认状态目录：local `%LOCALAPPDATA%\agentranch\local`，real `...\real`。
- **local/real 状态身份不同，不要复用账本**；状态目录含 SQLite/锁/身份文件，处理故障先备份。

## 4. 快速上手：跑出第一个真实结果页面（real 模式）

以已验证的 "design.md → 成都遛娃静态页" 闭环为例（run 61d0821d，四会话证据对等，详见附录 A）：

1. **创建项目**：GUI 内新建项目。
2. **导入设计文档**：UTF-8 `.md`/`.txt`，上限 256 KiB。检查显示的版本 U、文件名、完整文本、小写 SHA-256，确认无误后点"确认此上传版本"。版本/哈希过期时确认会被拒绝。
3. **拆分并审核任务**：逐项检查标题/描述/验收标准/依赖/公开输出，逐项"审核通过"；驳回必须写原因。
4. **手动开始开发** → **dispatch**：任务全部审核通过后点击开始与派发。真实链路随即执行：GUI → Paramiko 网关（固定 known_hosts、RejectPolicy）→ Linux Worker owner-exec → 模型调用 → artifact 回传 → Store admission。
5. **查看结果**：dispatch 完成后，artifact 落在 `<state-dir>/artifacts/<run_id>/`（index.html + js/ + css/，file:// 直开可看）。
6. **确认释放**：正常结束时 assignment 自动释放、used_slots=0；无需也不应手工释放。

## 5. CLI（aw-cli.exe）

```powershell
.\aw-cli.exe --help
.\aw-cli.exe check --config .\config.json            # 仅校验配置，不探测外部
.\aw-cli.exe local-check --state-dir "$env:LOCALAPPDATA\agentranch\local"
.\aw-cli.exe mcp --config .\config.json              # stdin/stdout 协议，勿混入日志
.\aw-cli.exe runner --config .\config.json --once
.\aw-cli.exe runner --config .\config.json --stop-on-stdin
.\aw-cli.exe controller --config .\config.json --once
```

- `--once` 与 `--stop-on-stdin` 互斥。
- CLI 非零退出码时保留 stdout/stderr 与状态目录；不要靠重跑或删账本把 UNKNOWN 当成功。
- real 模式不会静默回退 local；配置存在 ≠ 执行链就绪。

## 6. Linux Worker 侧配置（部署者）

real 模式需要受信任配置提供网关与 Worker 依赖。已验证部署形态（示例 .221）：

- **网关配置**（`native-ssh-gateway/1`）：host/port、ubuntu 普通用户、known_hosts 固定路径、worker ELF sha256/bytes、deployment 目录、`launch_mode`（direct / delegated-user-service）、可选 `remote_library_path/sha256/bytes`（LD_LIBRARY_PATH 注入）。
- **Worker 配置**（`native-worker-config/1`）：cgroup_root_mode（explicit / current-unit）、expected_worker_timeout_secs 等；字段校验规则见 `packaging/ssh-loader-path.txt`。
- **准入链**：worker ELF 身份 → signed start → 原生 owner-exec（cgroup 隔离）→ signed terminal evidence → Store admission → assignment 释放。传输层退出码/算术超时不释放槽位，Store 只认完整签名原生终态证据。
- **模型端点**：进程内调用，TLS 证书锁定（cert pinning），不落盘、不进 argv/日志。
- **秘密边界**：SSH 私钥与模型 token 通过 `AW-GATEWAY-SECRETS/1` 帧协议经 stdin 内存传递（见 §8），全程不落盘。

## 7. 可观测性与验收证据

真实会话的判定不靠界面绿灯，靠 evidence 链（`dist/` 会话目录，.gitignore 排除、本地保留）：

| 证据 | 含义 |
|---|---|
| `evidence/result.json` | harness 判定（PASS_MANAGER_PROJECT_CLOSURE） |
| `evidence/manager-state/manager-diag.log` | 3 事件（gateway_configured / dispatch_result / artifact_materialized），run_id 匹配 |
| `evidence/store-acceptance.json` | 独立复刻（inspect_project_store 重推）PASS |
| `evidence/manager-state/artifacts/<run_id>/` | 结果 artifact（含 sha256 链） |
| `evidence/browser-render-check.json` | 渲染核验 RENDER_OK（辅助 sanity） |
| `model_evidence`（DB） | 模型 status/elapsed/request·response sha256 |

验收工具：`tools/live_project_closure.py`（含 `inspect_manager_diag` diag gate、`inspect_project_store` 独立复刻）。回归基线：Rust 388/388、Python 119 passed 1 skipped。

## 8. 安全边界（务必遵守）

- 不要手工修改 SQLite、身份文件、文档哈希、审批字段来绕过阶段门槛。
- 不要把历史报告/旧 attempt/旧审批当当前执行证明。
- 关闭 GUI 只停本地工作台，不等于远端任务已取消。
- SSH：Paramiko RejectPolicy + 固定 known_hosts，不弱化、不 TOFU、不猜 key；禁止从旧日志恢复秘密。
- 模型 token 绝不进签名 envelope/gateway.json/数据库/evidence/日志。
- 未配置、未实现、执行失败、证据未建立是不同状态；按阻塞原因处理，不伪装成功。
- 已验证场景之外（其他主机/用户/模型端点/平台）一律视为未验证。

## 9. 常见问题

| 现象 | 处置 |
|---|---|
| 找不到 worker | 确认 `aw-cli.exe` 与 GUI 同目录同版，勿从 PATH 拼接 |
| 状态模式不匹配 | 保留原目录作证据，用正确 `--state-dir`，勿删文件强启 |
| real 未就绪 | 诚实失败路径：检查受信任配置、节点准入、AO/Reason 依赖，以实际探测与退出证据为准 |
| SmartScreen 警告 | 包未签名，属预期；无签名声明 |
| dispatch 后无 artifact | 检查 manager-diag.log 事件与 result.json status；中间截图外观不代表流程失败（已实证） |
| 中间状态"新项目"字样 | 输入框默认值，非占位符；不代表未创建 |

## 10. 已验证边界与未覆盖项

**已验证（evidence 背书，附录 A）**：Windows 原生 GUI 8 步控制序列 → Paramiko 网关 → .221 Linux Worker 原生 owner-exec → 真实模型调用（4 会话 response 互不相同）→ artifact 4 文件 → Store admission → assignment 自动释放 → used_slots=0 → manager 干净退出；diag gate（manager-diag.log 3 事件）真实会话端到端通过；独立复刻与双向回验 4 会话 ALL_MATCH。

**明确未覆盖**：其他 Linux 主机/用户/平台；installer/签名；Linux 构建 Manager；多并发用户场景；模型端点故障注入；长期运行稳定性。这些均为 NOT_RUN，不以固定样例伪装完成。

## 附录 A：验证证据索引

| 会话 | run_id | 判定 | 关键证据（dist/ 目录，本地保留） |
|---|---|---|---|
| -41 | ea3aed16 | 业务级 PASS（独立复刻） | store-acceptance.json |
| -43 | bed090c1 | harness PASS | result.json + 独立复刻 |
| -44 | 16e57efa | harness PASS + diag 3 行 | result.json + manager-diag.log |
| -47 | 61d0821d | harness PASS + diag gate PASS + 质量/渲染对等 | result.json + browser-render-check.json |

（四会话 4×4 artifact 文件 sha256 两两互异；response_sha256 互不相同 → 真实生成非复制。）

## 附录 B：术语

| 术语 | 含义 |
|---|---|
| Store | worker 侧准入/槽位账本；只认完整签名原生终态证据 |
| assignment / used_slots | 一次派发的占用与释放；正常闭环自动归零 |
| signed start/terminal | Worker 启动/终态的签名证据帧 |
| owner-exec | Worker 以资源属主身份原生执行业务子进程的入口 |
| diag gate | harness 对 manager-diag.log 三事件的永久门槛 |
| 冻结包 | source-inventory + build-identity + 双 exe 同目录的不可变交付目录 |
