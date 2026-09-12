# HANDOFF — workbuddy-switch

> 更新：2026-09-12 18:54 · 分支 dev · 工作区干净 · `9af08ef`
> 本阶段：**积分口径三连突破** —— ① join key 修正（traceId→conversationRequestId，100% 命中）
> ② 计费机制定论（主人亲证：无共享池，**每模型每日限额，超额禁用**；0 倍率=Hy3 限时免费）
> ③ **官方逐笔明细按账号落盘**（credit_ledger，与官方接口完全一致、不受切号对齐影响）
> ✅ 已竟：按账号×日×模型折线图（`reports/credit_ledger_charts.html`，4 张纯 SVG 零依赖）
> ✅ 已竟：每模型单日最高用量表（`reports/model_max_daily.md`）+ 手动用量检查器（`scripts/analysis/model_daily_limit_check.py`）+ 一键 cmd（`scripts/check_daily_limit.cmd`）
> ⚠️ 待办：WorkBuddy **5.5.6** 更新包已下载但**未安装**（见「下一步」5）

## 进度（现在在哪）

- **dev = `9af08ef`**（本会话 5 提交：`1788d89` 折线图 → `c74c1e3` 用量检查脚本 → `8f37969` 一键 cmd → `7c45eb4` cmd 编码修复 → `9af08ef` 输出改逐账号表格）
- 验证：本会话仅改 Python 脚本 + .cmd（无 Rust/TS 改动），`cargo test`/`tsc` 门不适用；脚本均实跑通过；pre-commit 13 项 PASS；全部已 push
- 本阶段产出（分析脚本在 `scripts/analysis/`，Rust 模块在 `wb-switch-core/src/modules/`）：
  1. `session_cost.py`：按对话 Token/命中率/积分；`--by-account` 时间轴归因；`--by-model` 账号×模型双口径（total + 计费口径）
  2. `official_vs_local.py`：官方账单 vs 本地记账逐格对照 → **本地 session_usage 只记超额段（4 天 21 格仅 2 格吻合）**，官方每笔都扣
  3. `ledger_stats.py`：读账本按账号×日×模型统计 —— **按账号积分归属的权威口径**
  4. `credit_ledger.rs`（本地专属，上游零冲突）：官方逐笔明细按账号落盘 `~/.wb-switch/credit_ledger/<accountId>.jsonl`（requestId+ts 去重、180 天清理），统计页点刷新自动触发，已真机验证 4 账号
  5. `generate_ledger_charts.py`：读账本 → 4 张纯 SVG 折线图（`reports/credit_ledger_charts.html`，零外部依赖离线可看）：每账号一张（每线=一模型，X=日，Y=当日扣分）+ 一张三账号每日总扣分对比。口径直接取官方明细，不重算
  6. `model_max_daily.md` + `model_daily_peak.md`（reports/，不入库）：每模型单日最高用量排序表（跨账号取最多的一天）—— **每天一个模型最多能用多少分的唯一可靠口径**
  7. `model_daily_limit_check.py`：只盯 deepseek-v4.1-flash / glm-5.3-flash；今日用量按账号日累计、取单账号单日最大值对比峰值基线；超峰值自动更新 `~/.wb-switch/model_daily_peaks.json`
  8. `check_daily_limit.cmd`：双击即跑上脚本（纯 ASCII + CRLF + `chcp 65001`，避开 GBK 乱码）
  9. 账本全景（31 天窗口）：**真实扣分 17,184 积分**（H 7,832 / Harvey 6,248 / Elaine 3,104），大头是 deepseek-v4-flash 10,374（60%，8 月旧模型，9 月已切 ds-v4.1-flash 0.03x）
- 关键认知修正（主人 16:51 亲证）：**没有账号级共享额度池**；收费标准各账号一样（单笔均价 H 4.01 ≈ Elaine 4.02）；**每个模型有每日限额，超额禁用只能换模型**；当前仅 Hy3 0.00x 限时免费
- ⚠️ **H 余额告急**：剩 239.7/4765（95% 已用，09-12 17:04 快照）；Elaine 1,891.6/4,927 · Harvey 941.3/4,588 · 廿七 2,100/2,100（新号未用）

## 决策（为什么这样做）

- **统计口径一律复用项目实现，禁止 Python 复刻**：复刻版算 8,530 条 vs 项目实际 8,554 条，偏差数百条且难定位 → 改为 `examples/dump_stats.rs` 直调。
- **按对话统计，不按账号**（主人 2026-09-11 明确）：账号维度（额度紧张度等）降为次要信息，不作诊断主口径。
- **积分不绕 traceId join**：`session_usage` 表天然按会话存（`session_id` 与 JSONL 文件名实测 42/42 对应），直接查表即可；旧方案「join JSONL」已废弃。
- **Python 脚本只做聚合，不重算项目口径**：token/命中率由 Rust 侧产出 JSON，Python 只负责与积分合并渲染。
- **`reports/` 不入库**：含本机项目名/路径，且可由脚本随时重现。
- **限额监控自动标定不可靠（主人 18:21 拍板）**：「某日某模型 credit 突停+换模型=触顶禁用」信号被否——账本是账单记录不含换模型原因，且被三种情况污染：①没到量主动换 ②并行多任务多模型（实证 H 09-10 单日同跑 ds-v4.1-flash719+glm-5.3374+minimax-m3130）③免费模型 hy3 credit 恒 0 信号失效。→ 每模型单日最高用量（`model_max_daily.md` 峰值表）是**唯一可靠**的「每天最多用多少分」；若产品化监控，只能用「同日 plateau+其他模型仍活跃」软信号且标「疑似」，排除免费模型。
- **手动用量检查器口径**：今日用量按账号日累计、取「单账号单日最大值」对比全局峰值基线（与峰值表一致）；基线 `~/.wb-switch/model_daily_peaks.json` 只增不减，超了就更新。不接入项目页面，纯手动（主人要求）。
- 历史决策（账号发现数据源 / 对齐分层 / 云端自动化放弃 / vite 白壳 issue #30）见 `docs/PROGRESS.md`，此处不重复。

## 坑位（别再踩）

1. **统计口径三条公式**（权威：`crates/wb-switch-core/src/modules/token_stats.rs`）
   - `total = input + output + cacheWrite`（**input 已含 cacheRead，不重复加**）
   - 命中率 `= cacheRead / input`
   - 用量优先级 `message.usage` > `providerData.usage` > 顶层 `usage`；**必须有 input 字段存在**才算有效记录；`rawUsage` 只兜底 cacheWrite
   - **一个 JSONL 文件 = 一个对话**；排除 `subagents/`
2. **AI 沙箱里 server 起不来**：`wb-switch-rust.exe` 是 GUI 版（启动报 crashpad）；server 是 `wb-switch.exe`，但在沙箱里后台启动后**不监听端口** → 取数一律走 `examples/dump_stats.exe`，别折腾 HTTP。
3. **example 产物路径**：`target/debug/examples/dump_stats.exe`；编译命令需带 GNU 三件套（`RUSTFLAGS=-C link-arg=-fuse-ld=lld` + `w64devkit/bin` 入 PATH），增量约 15s。
4. AI 沙箱读不了宿主 `CodeBuddyExtension/.../auth`（os error 5）→ 需放行前台命令；主人正常环境无此问题。
5. 版本判定：真实版本只看 `resources/install-manifest.json` 的 `appVersion`；安装目录 `version` 文件是 **Electron 内核版本**，`cli/vendor/sandbox/<x>/` 是 **sandbox 组件版本**，都不是 WorkBuddy 版本。
6. `.asar` 是 Electron **归档非加密**（头部明文 JSON 索引）→ 可解包，但 WorkBuddy 仍闭源，受 License 约束。
7. 大仓库 git 写操作（checkout/merge/gc）**不在 2 分钟前台超时窗口内跑**（曾两次损坏 `.git`）；`gc.auto=0` 勿改回。关键节点必须 push。
8. 编译前停掉运行中的 `wb-switch.exe` / `wb-switch-rust.exe`。
9. **`.cmd` 必须用纯 ASCII + CRLF + 开头 `chcp 65001`**：UTF-8 无 BOM 中文在 GBK 代码页 cmd 下乱码，连 `cd`/`set` 等 ASCII 行也被误解析报「不是内部或外部命令」。中文输出交给 python（控制台 65001 即正确显示）。
10. **账本聚合必须先按 (账号,日,模型) 汇总日累计再取 max**：直接拿逐行 credit 比大小会退化成「单笔最大」（初版播种出 23.5/83.6 而非 719.1/806.1），抽 `daily_model_credit()` 先汇总。
11. **Git Bash 下 `$USERPROFILE` 路径会被 mangled**（`d:\...\C:\Users\WH\...`）：删/读 `~/.wb-switch` 下的文件一律用 Python `os.path.expanduser("~")`，别用 shell 变量。

## 下一步

1. ✅ **（已完成）按账号×日×模型积分折线图**：`reports/credit_ledger_charts.html`（4 张纯 SVG 零依赖）。重跑：`python scripts/analysis/generate_ledger_charts.py`
2. **用量监控：手动版已完成，产品化待定** —— 手动检查器（`scripts/analysis/model_daily_limit_check.py` + 双击 `scripts/check_daily_limit.cmd`）已可用，输出「基线+逐账号」表格，超峰值自动更新 `~/.wb-switch/model_daily_peaks.json`。**自动标定真实每日限额不可靠**（见决策 限额监控自动标定不可靠），若产品化只能做软信号且标「疑似」、排除免费模型；可选：① 接成定时任务每天自动跑 ② 切号对话框加「余额<20% + 单模型逼近峰值」提醒（H 已 95%）
3. **账本 UI 化（可选）**：统计页加「按账号扣分」视图，读 `credit_ledger/` 即可（口径与官方一致）
4. **`credit_diagnose.py` 已保留**（撤销 9-11「可删」判断；账号×模型归因依赖它共享的 timeline 模块）——旧待办作废
5. **安装 WorkBuddy 5.5.6**（环境待办）
   - 包已就位：`C:\Users\WH\AppData\Local\Temp\workbuddy-update-x64\WorkBuddy-Setup-5.5.6.38337834.exe`
   - 双击安装 → 重启 WorkBuddy → 设置里「检查更新」应显示已最新
   - **装前建议**：`git tag pre-5.5.6` + 快照 `~/.wb-switch/`；装后**必须回归**两项：① 切号主题跟随 ② 切号对齐勾选（新版可能改 LevelDB / db 结构，`ui_theme.rs` 注入逻辑或需同步调整）
   - 若安装无反应/版本未变：查火绒实时防护是否拦截写 `C:\Program Files\WorkBuddy\`
6. **按对话统计并入 webui**（可选）：需新增 Rust 侧「积分按会话」接口（读 `session_usage`）+ 前端页面
7. **issue #30 跟进**：https://github.com/changexbc/workbuddy-switch/issues/30 —— 维护者积极则按 ①vite →②账号发现 →③数据对齐 顺序提 PR
8. **项目 memory 未入库**：`.gitignore` 含 `/.workbuddy/`，`.workbuddy/memory/*.md` 不入库（跨会话交接物之一）。若要入库需调整该条忽略规则 —— 待主人决定
9. 备选：`wb_multi_sync` 退役（L4/L5 已内置）；主题皮肤加载闪烁根治依赖官方（issue #93057）
10. **重置锚点已实弹确认（2026-09-12 19:11）**：ds-v4.1-flash 每日限额**固定重置于每天 14:26:22（北京时间）**，配额窗口 `[14:26:22 → 次日 14:26:22)`；非日历零点、非触顶后滚动 24h（H2 已被三账号账本否决）。验证脚本 `scripts/analysis/verify_reset_anchor.py` 可复跑。**待办（B）**：`model_daily_limit_check.py` / `model_daily_peaks.json` 当前按日历日 00:00 聚合，与真实 14:26:22 窗口错位 → 需改为按 14:26:22 边界重窗，峰值表才与真实窗口对齐（跨边界用量当前会被拆到两日历日，既漏报窗口级超额又低估上限）。
