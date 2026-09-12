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

## 2026-09-12 积分可视化与用量监控（续三连突破）

- **HANDOFF 下一步 1 完成（折线图）**：新增 `scripts/analysis/generate_ledger_charts.py` 读 `credit_ledger/*.jsonl` 官方明细，输出 `reports/credit_ledger_charts.html`（4 张纯 SVG 零依赖）：每账号一张（每线=一模型，X=日，Y=当日扣分）+ 三账号每日总扣分对比。数据对齐全景 17,184 积分（H 7,832 / Harvey 6,248 / Elaine 3,104）。commit `1788d89`。
- **每模型单日最高用量表**：`reports/model_max_daily.md`（跨账号取最多的一天）。口径 = max over (账号×天) 的当日扣分，是「每天一个模型最多能用多少分」的唯一可靠口径。峰值：ds-v4-flash 1297.1 / glm-5.3-flash 806.1 / ds-v4.1-flash 719.1 / glm-5.2-x 541.5 / glm-5.3 374.2 / glm-5.2 360.6 / minimax-m3 129.6 / 其余 <130；hy3 恒 0（0.00x 免费）。
- **限额监控启发式被否（决策）**：原计划「某日某模型 credit 突停+换模型=触顶禁用」不可靠 —— 账本是账单不含换模型原因，且被三种情况污染：①没到量主动换 ②并行多任务多模型（H 09-10 单日同跑 ds-v4.1-flash719+glm-5.3374+minimax-m3130）③免费模型 hy3 credit 恒 0 信号失效。→ 峰值表作唯一可靠基线；若产品化只能用「同日 plateau+其他模型仍活跃」软信号且标疑似、排除免费模型。
- **手动用量检查器（主人要求，不接入页面）**：`scripts/analysis/model_daily_limit_check.py` 只盯 deepseek-v4.1-flash / glm-5.3-flash，今日用量按账号日累计取单账号单日最大值对比基线，超峰值自动更新 `~/.wb-switch/model_daily_peaks.json`；输出「基线+逐账号」表格（百分比按账号各自算）。配套 `scripts/check_daily_limit.cmd` 双击即跑。commit `c74c1e3`→`8f37969`→`9af08ef`。
- **两个 bug 已修**：① 播种直接拿逐行 credit 比大小退化成「单笔最大」（23.5/83.6 而非 719.1/806.1）→ 抽 `daily_model_credit()` 先汇总日累计再取 max；② `.cmd` UTF-8 无 BOM 中文在 GBK cmd 乱码报「不是内部或外部命令」→ 改纯 ASCII + CRLF + 开头 `chcp 65001`。
- **待办**：WorkBuddy 5.5.6 更新包已下载未安装（环境项）；用量监控产品化待主人拍板。


## 2026-09-12 6004 频率限制机制破案（滑动窗口定案）

- 主人三连纠正驱动：①「sessions.user_id 全是 Elaine = 每次切号 L3 对齐改写归属，须按账号活跃时间段区分」②「全网搜 6004 相关资料」③「hy3 重置不到 3h」。
- **全链路关联**：6004 日志文件名 = `sessions.id`（零时间比对 join）；账号归因 = `~/.wb-switch/backups/workbuddy-desktop.*.info`（28 个备份，UTC 时间戳 + `account.uid`）按"≤触发时刻最后备份"二分。备份只覆盖 09-10 21:49 后，此前 6 次 6004 无法归因。
- **撤案**：「13 个账号」（32hex 是请求相关性 ID 非账号 ID，同对话多值即证）、「user_id 是人类用户」（是 L3 对齐痕迹）、「固定每日锚点 14:26:22 / 每账号一锚」（归因后同账号同模型双锚点即否）。
- **定案**：6004 = 滑动窗口请求数限流；重置时刻 = 容量绑定请求（窗口内第 C−N+1 旧）+ 窗口长 W。铁证：3 锚点 −24h 与真实 sendPrompt ±5s 吻合（00:37:33 / 20:56:45 / 14:26:22）。ds-v4.1-flash W=24h；hy3 短窗（≈3h 或 4.5h 候选，待下次触发定案）。同绑定未滑出前重触发重置不变（hy3 20:32/20:48 同 22:59:49 实证）。
- **全网检索**：官方错误码文档仅"切换模型重试"零机制；腾讯云社区"重置时间系统动态计算"与本地模型互证；php.cn"北京 12 点刷新"与秒级实测矛盾判低质不采信。本地结论全网最高精度。
- 新增 `scripts/analysis/find_6004_events.py`（检测/join/归因/解窗一体化）；HANDOFF 14:26:22 固定锚结论同步作废。
- 待解：credit_ledger 三主账本 14:26:22 边界与滑动窗的兼容性；hy3 W 定案。

## 2026-09-12 会话归属对齐排除软删 + 项目侧栏同步/会话瘦身（路线替代上游复制会话）

- **会话归属对齐排除软删**（b67e8bd）：主人确认已删对话不参与新对话上下文（每对话独立 JSONL），对齐改归属无意义 → align.rs sessions 查询加 `deleted_at IS NULL`。
- **上游调研否决复制路线**：#32（open）复制会话 usage 落盘新 jsonl → token_stats 无去重，作者本机 67% usage 重复、某日虚高 22.7 倍；#9（closed 被拒）复制前去重+存量清理被 maintainer 否。上游 main 现状：复制不去重、统计不去重。本机实测仅 0.7% 重复（集中在 f4413cf7 单文件，没用过复制功能）。
- **定案（主人多轮收敛）**：切号不复制/不搬运会话正文。连续性 = 设置同步 + 项目占位锚点 + 空锚点续聊；每项目至少一个占位会话，删项目后其他账号切号以上个账号项目清单为准「少的补、多的删」。
- **新增 `projects_anchor.rs`**（本地专属零冲突）：`sync_project_set_in_db`（补缺 INSERT 占位会话+空 JSONL / 删多软删 / 快照级联防护：清单清零或骤减 30% 中断，force 放行）+ `slim_sessions_in_db`（每 cwd 留 updated_at 最新 N 条）+ `workspace_dir_name`（盘符小写+:删+斜杠转-，33 目录反推验证）。快照 `~/.wb-switch/project_set_snapshot.json` 路径注入可测，6 单测。
- **命名改造（主人指定）**：数据文件一致性同步 → **设置同步**（吸收主题跟随 → report.settings.theme，报告键 files→settings）；自动化归属对齐 → **定时任务迁入**；会话归属对齐 UI 下线。post_close_sync 签名改 `(target_acc, &AlignOptions)`；SwitchOptions 加 sync_projects/slim_keep。
- 双门：cargo 194 绿 + tsc 0 错；115daef 已推 origin/dev，pre-commit 全 PASS。
- 与 L4 设置同步管辖域无交集（L4 管 claw.users/SECRET_KEYS/storage，不管 sessions/cwd）→ 无需改动。
- 待主人真机验证：占位会话在 WB 侧栏显示、可点开续聊。

## 2026-09-12/13 白屏终局：vite 8.3.0 rolldown 内核根治 rollup 双 React 实例（49182b1）

- 现象：webui/桌面 debug+release 全白屏，`Cannot read properties of null (reading 'useRef'/'useState')`（TooltipProvider/App/UpdateCenter 等）。
- **三因素叠加**（各自独立都会白屏）：① debug tauri 壳走 devUrl:1420 需 vite dev 常驻（run-dev.cmd 固定等 10s 但 vite 冷启动 ~53s → 8c46a68 改轮询 90s + 落日志）；② esbuild 0.28.2 minify 破坏 React 19 产物（c1884fb 关 minify）；③ **真凶（终局）**：rollup 在 Windows 下对 react CJS 包生成两个模块实例（react_production / react_production$1），部分组件绑到无 dispatcher 的副本 → hooks 返回 null。升级 **vite 8.3.0（rolldown 内核）** 后产物结构正常（import_react 统一绑定）→ 主人真机确认「可以了」。
- 配套防御（49182b1）：react/react-dom/vite pin 精确版本；vite.config minify:false + react 正则 alias（对象形式 `react:` 前缀会劫持 react/jsx-runtime → 必须正则数组）+ commonjsOptions strictRequires；tauri 加 devtools feature（真机 console 诊断）。
- 排查方法论：alias 修复曾假阳性——esbuild minify 改名骗过 `var react_production` 计数（零命中是改名了）→ **验证产物要看 import 结构，不看变量名计数**；「dev 正常 vs build 崩」对照直接锁死构建环节；WebView2 缓存（EBWebView，已备份清理）与代码问题无关。
- 上游官方无此问题：Linux CI 大小写敏感文件系统行为不同。dev 正常/release exe（wb-switch.exe 01:29 / wb-switch-rust.exe 01:30 构建）均验证通过。
