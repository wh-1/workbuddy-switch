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
- 结果：dev 合并完成，176 测试全绿 + tsc 绿 + cargo check 绿；dev/main 均已 push 到 origin。

## 2026-09-10 收尾段（统计实证 / 替代分析 / 上游 issue #30）

- **Token/积分统计机制 + 删对话实证**：Token 统计纯派生（扫 `projects/**/*.jsonl` 排除 `subagents/`，无账本）；积分统计靠 `credit_usage_snapshots.json` 相邻快照下降差 + 官方账单接口。实证：本机 20 个已删会话（`deleted_at` 非空）JSONL 全部仍在 → 今日 174M token 中含已删会话 6M 仍计入 → **删对话（软删）不掉 Token 统计**；WorkBuddy 删对话只清 `session_usage` 表（本项目不读）。
- **替代关系确认**：git diff + 双子代理比对 `wb_multi_sync`，确认 dev 改动**完全替代**且为超集（L1/L3/L4/L5 全覆盖 + token/积分/签到/旅行等增量）；wb_multi_sync 可退役（保留仓库归档）。
- **改动完整性**：全量 grep `TODO|FIXME|unimplemented!` 零真缺口；前端 45 命令 ↔ 后端 41 路由 ↔ Tauri 注册全对齐；硬指标 176 绿 / tsc 绿 / cargo check 绿。
- **回贡上游评估**：推荐推 ①vite 双栈修复 ②账号发现 ③数据对齐（由小到大建立信任）；不推 AGENTS/HANDOFF/memory/版本号。
- **vite 白壳根因（Windows 实测）**：上游 `host: host || false` → 只绑 `[::1]:1420`；Windows `localhost` 优先 IPv6 → WebView2（IPv4）连不上 → 白壳。本地 `host: host || true` 双栈修复。仅影响 Windows dev 环境，release 包内嵌 dist 不受影响。
- **issue #30 已提交 + 严谨核实**：https://github.com/changexbc/workbuddy-switch/issues/30（作者 wh-1，状态 open）。决策：只开 issue 不建 PR。三项对照实验（false→仅::1 / true→0.0.0.0+[::] / 127.0.0.1→127.0.0.1）证实根因；全网检索命中 Tauri #9509、Vite #16522、官方模板即 `host: host || false`，方案有社区共识。PATCH 增强正文至 1873 字符。
- **新建两个跨项目 skill**：`git-corruption-rescue`（git 仓库损坏抢救流程）、`github-api-without-gh`（无 gh 时 PAT 直连 GitHub REST API）。
- 收尾时 dev = `6fc8ca5`（已含 HANDOFF 路径迁移至归档区、接入 w-dev 项目守卫），工作区干净；176 测试 + tsc 复核全绿。
