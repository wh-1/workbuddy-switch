# PROGRESS — workbuddy-switch

> 历史归档：每阶段一节，**只追加不改**。当前状态见 `HANDOFF.md`；规则见 `AGENTS.md`。

## 2026-09-09 官方 v0.1.34 基线与多账号数据全量对齐

- 合并官方 v0.1.34 修复（Windows CodeBuddy CN 按 PID 关停、macOS 钥匙串零弹窗）为基线。
- 新增 `align.rs`（L1 备份 / L3 会话归属 / L4 文件一致性 SECRET_KEYS 25 键 / L5 my-files 并集 / dry-run 预览），替代 wb_multi_sync。
- `switch_account` 参数收敛为 `SwitchOptions`；新增 `align_data` / `align_automations` 独立命令（Tauri + server）。
- 前端：切号弹窗加「自动化跟随切换 / 会话归属对齐 / 数据文件一致性」开关 + 「对齐预览」。
- 期间事故：合并被 SIGTERM 打断致 .git 损坏（refs/objects 丢失），已重建并全量还原；疑似 AV 误删含凭据关键词源码。仓库 remote 统一改 SSH。
- 结论：157 测试全绿；dev 分支承载开发，main 只跟上游。

## 2026-09-09 对齐功能 API 实测（CLI server）

- 增量重编 server/Tauri 后跑通：`/api/accounts`、`/api/status`、`/api/align/data`（dry-run）。
- dry-run 预览（target=Elaine）：automations 9 条、sessions 54 条（含软删口径，未删 36）、storage 复制 1 文件；复核 db 零落盘。
- 口径备忘：sessions 对齐含 `deleted_at` 非空行；automations 只算未删。

## 2026-09-10 账号发现功能（扫本机登录历史自动识别）

- 问题：账号库只认 `~/.wb-switch/accounts.json`，本机实际登录过 3 个账号只显示 2 个。
- 新增 `discover.rs`：扫官方 auth 目录历史备份（`workbuddy-desktop.*.info`，排除当前登录态）按 uid 取最新 + 合并 storage/settings/memory 残留 uid → 输出 `source`（auth-history/residual）、`restorable`、`inAccountList`。
- `adopt_account`：用最新备份构造账号记录入库，写前备份 accounts.json。
- API：`GET /api/accounts/discover` + `POST /api/accounts/adopt`；Tauri 命令 `discover_known_accounts` / `adopt_account`；前端账号页「发现曾登录账号」提示条 + 一键补录。
- 结果：3 账号（H / Elaine / Harvey）全部识别并补齐在册；159 测试全绿 + tsc 绿。
- 顺带修复：`vite.config.ts` 双栈监听（`host: true`）根治 Tauri dev 白壳。
- 环境发现：AI 沙箱长驻进程读不了宿主 auth 目录（os error 5），放行前台命令可读；正常使用无影响。

## 2026-09-10 合并上游 v0.1.36（含 .git 二次损坏恢复）

- 上游新增 6 提交（v0.1.34 → v0.1.36）：`travel` 派猫猫旅行自动派发/领取（`travel.rs` +1236 行）、Token 统计新增 **CodeBuddy IDE 来源**（读 CodeBuddyExtension history index 的 `requests.usage`，不扫消息正文）、版本号 0.1.36。
- 合并：main ff 到 `bbb0d3c`；dev 合并产生 3 处冲突（`crates/wb-switch-server/src/api.rs`、`src-tauri/src/commands.rs`、`src/lib/api.ts`），均为模块/命令注册表，按「两边都保留」解决（本地 `align`/`discover` + 上游 `travel`）。
- **二次事故**：合并再次被 SIGTERM 强杀 → `.git/refs/` 消失、loose objects 归零、旧 pack（27MB 全史）`.pack` 被删、工作区 59 文件消失。恢复手段与损失范围见 `HANDOFF.md`「事故」段；实际损失仅为 5 个本地提交的历史粒度，内容零损失。
- 防护升级：仓库 `gc.auto=0` + `gc.autoDetach=false`；**约定大仓库 git 写操作一律后台或超长超时执行，禁止在 2 分钟前台超时窗口内跑 merge/checkout**。
- 结果：dev = `a07eac0`，176 测试全绿 + tsc 绿 + cargo check 绿；dev/main 均已 push 到 origin。
