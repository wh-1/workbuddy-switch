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

## 2026-09-11 按对话统计工具（Token / 命中率 / 积分）

- **起因排查**：主人问「已是 5.5.4 为何仍提示更新」→ 证实服务端确有 **5.5.6.38337834**，更新包已下载到 `%TEMP%\workbuddy-update-x64\`，日志只有 `ready` 无 install 事件 → **下载完成但未安装**（`WorkBuddy.exe` mtime 仍是 5.5.4 安装时间）。顺带厘清版本判定：真实版本只认 `resources/install-manifest.json` 的 `appVersion`；安装目录 `version` 文件是 Electron 内核版本，`vendor/sandbox/<x>/` 是 sandbox 组件版本。
- **认知澄清（非代码改动）**：`resources/app.asar` 是 Electron **归档格式非加密**（296MB，头部明文 JSON 索引）→ 「可读」≠「开源」，WorkBuddy 仍为闭源商业软件，保护靠 License。主人提出「单主账号使用其他账号积分」经论证**不可实现**（积分记账与 token 身份强绑定，服务端唯一可审计方式），已说明并劝退。
- **口径纠正（关键）**：初版诊断脚本按**账号**拆分消耗，主人指出方向错误 —— 应**按对话**统计，且「项目都算好了」。改为完全复用项目实现。
- **新增 `crates/wb-switch-core/examples/dump_stats.rs`**：诊断用 example，直接调 `token_stats::get_statistics()`，支持 `[days]` / `--sessions` / `--json <path>`。**教训：禁止用 Python 复刻统计口径** —— 复刻版算出 8,530 条，项目实际 8,554 条，偏差数百条且难定位；直调项目实现后口径与产品页一致。
- **统计口径固化为三条**（`token_stats.rs`）：① `total = input + output + cacheWrite`（input 已含 cacheRead，不重复加）；② 命中率 `= cacheRead / input`；③ 用量取值优先级 `message.usage` > `providerData.usage` > 顶层 `usage`，且必须有 input 字段存在才算有效记录（`rawUsage` 仅兜底 cacheWrite）。**一个 JSONL 文件 = 一个对话**。
- **积分口径修正**：`workbuddy.db` 的 `session_usage` 表**天然按会话存储**（每行 = 一个会话，`session_id` 与 JSONL 文件名实测 **42/42 完全对应**），`credit_json = {traceId: 积分}` 求和即该对话积分。**推翻上一版"需 traceId join JSONL"的认知** —— 直接查表即可。
- **新增 `scripts/analysis/session_cost.py`**：按对话输出 Token / 命中率 / 积分三指标（积分跨行 traceId 取最大值防重复累加）。配套 `scripts/analysis/credit_diagnose.py` 降为辅助（含积分时间分布/模型/项目维度）。
- **实测（近 30 天）**：58 个对话 · Total 1.70B · 调用 8,563 次 · 命中率 96.0% · 积分 1,616.23（仅 13 个对话有记账）。全量：71 对话 · 1.81B · 9,695 次 · 95.9% · 2,592.08（20 个对话有记账）。**最烧对话**：daily_stock_analysis「对比方案设计」214.65M / 97.0%（积分 0，走免费额度）。
- **结论**：「积分不够」非当前瓶颈 —— 免费/套餐内额度不产生积分记账，近 30 天 87% 请求积分为 0 属正常。

