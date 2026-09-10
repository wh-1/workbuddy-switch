# HANDOFF — workbuddy-switch

> 更新：2026-09-11 凌晨 · 分支 dev · 工作区干净
> 上阶段：合并上游 v0.1.36 + 切号对齐修复 + 主题跟随账号（均已 GUI 实测通过）
> 本阶段：**按对话统计工具**（Token / 命中率 / 积分）—— 口径完全复用项目实现
> ⚠️ 待办：WorkBuddy **5.5.6** 更新包已下载但**未安装**（见「下一步」1）

## 进度（现在在哪）

- **dev = `e70a515`**（本阶段单提交；上一节点 `6f403d2` = 9/10 收尾文档）
- 验证（收尾复核）：`cargo test -p wb-switch-core` **182 全绿** · `npx tsc --noEmit` 绿 · pre-commit 项目体检 **8/8 PASS**
- 本阶段产出：
  1. `crates/wb-switch-core/examples/dump_stats.rs` — 诊断 example，直调 `token_stats::get_statistics()`，支持 `[days]` / `--sessions` / `--json <path>`
  2. `scripts/analysis/session_cost.py` — **主脚本**：按对话输出 Token / 命中率 / 积分
  3. `scripts/analysis/credit_diagnose.py` — 辅助：积分时间分布 / 模型 / 项目维度
  4. `.gitignore` 新增忽略 `reports/`（产物可重新生成）
- 实测（近 30 天）：**58 对话 · 1.70B token · 8,563 调用 · 命中率 96.0% · 积分 1,616.23**（仅 13 个对话有积分记账）
- 环境事实：本机 WorkBuddy 当前 **5.5.4**；官方 **5.5.6.38337834** 更新包已下载至 `%TEMP%\workbuddy-update-x64\WorkBuddy-Setup-5.5.6.38337834.exe`（532MB），日志只有 `ready` 无 install 事件 → **未安装**
- **main = `bbb0d3c`**（上游 v0.1.36）；origin 双备份；构建形态仍是 debug exe + vite，**无 release 包**

## 决策（为什么这样做）

- **统计口径一律复用项目实现，禁止 Python 复刻**：复刻版算 8,530 条 vs 项目实际 8,554 条，偏差数百条且难定位 → 改为 `examples/dump_stats.rs` 直调。
- **按对话统计，不按账号**（主人 2026-09-11 明确）：账号维度（额度紧张度等）降为次要信息，不作诊断主口径。
- **积分不绕 traceId join**：`session_usage` 表天然按会话存（`session_id` 与 JSONL 文件名实测 42/42 对应），直接查表即可；旧方案「join JSONL」已废弃。
- **Python 脚本只做聚合，不重算项目口径**：token/命中率由 Rust 侧产出 JSON，Python 只负责与积分合并渲染。
- **`reports/` 不入库**：含本机项目名/路径，且可由脚本随时重现。
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

## 下一步

1. **安装 WorkBuddy 5.5.6**（环境待办，最高优先）
   - 包已就位：`C:\Users\WH\AppData\Local\Temp\workbuddy-update-x64\WorkBuddy-Setup-5.5.6.38337834.exe`
   - 双击安装 → 重启 WorkBuddy → 设置里「检查更新」应显示已最新
   - **装前建议**：`git tag pre-5.5.6` + 快照 `~/.wb-switch/`；装后**必须回归**两项：① 切号主题跟随 ② 切号对齐勾选（新版可能改 LevelDB / db 结构，`ui_theme.rs` 注入逻辑或需同步调整）
   - 若安装无反应/版本未变：查火绒实时防护是否拦截写 `C:\Program Files\WorkBuddy\`
2. **（可选）按对话统计并入 webui**：把 Token/命中率/积分三列做成产品页 → 需新增 Rust 侧「积分按会话」接口（读 `session_usage`）+ 前端页面
3. **（待定）`credit_diagnose.py` 去留**：主人已明确不关注账号维度，可删（`scripts/analysis/credit_diagnose.py`）
4. **issue #30 跟进**：https://github.com/changexbc/workbuddy-switch/issues/30 —— 维护者积极则按 ①vite →②账号发现 →③数据对齐 顺序提 PR
5. **项目 memory 未入库**：`.gitignore` 含 `/.workbuddy/`，导致 `.workbuddy/memory/*.md` 不入库（跨会话交接物之一）。若要入库需调整该条忽略规则 —— 待主人决定
6. 备选：`wb_multi_sync` 退役（L4/L5 已内置）；主题皮肤加载闪烁根治依赖官方（issue #93057）
