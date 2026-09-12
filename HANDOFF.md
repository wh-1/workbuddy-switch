# HANDOFF — workbuddy-switch

> 更新：2026-09-12 21:22 · 分支 dev · `7843ea2` + 收尾提交
> 本阶段：**6004 频率限制机制破案** —— ① 6004→对话→账号全链路打通（零时间比对）
> ② 推翻"固定每日锚点"，定案**滑动窗口限流**（重置 = 绑定请求 + 窗口长，±5s 逐秒验证）
> ③ 全网检索确认：官方无口径，社区"动态计算"与本地结论互相印证
> ✅ 新增 `scripts/analysis/find_6004_events.py`（6004 检测/归因/解窗一体化）

## 进度（现在在哪）

- **dev = `7843ea2`** + 本收尾提交（新增 `find_6004_events.py`；本会话其余产出全在 memory，不入库）
- 验证：本会话仅改 Python 脚本 + memory（无 Rust/TS 改动）；`cargo test` + `tsc --noEmit` 双门绿（收尾时复跑确认）
- **6004 全链路关联方法（全部实测验证）**：
  1. **事件→对话**：6004 在 `~/.workbuddy/logs/<日期>/sdk/conversations/<convId>.log`，**文件名 = `workbuddy.db`.`sessions.id`**，直接 key join 取标题/模型
  2. **对话→账号**：`sessions.user_id` 被 wb-switch 对齐 L3 **改写为当前账号**（全表仅 1 值 = Elaine 的对齐痕迹，不可用）→ 真相源 = `~/.wb-switch/backups/workbuddy-desktop.*.info`（28 个，文件名 UTC 时间戳 + 文件内 `account.uid`），按"≤ 触发时刻的最后一个备份"二分归因。限制：备份只覆盖 09-10 21:49(UTC+8) 之后，之前的 6 次 6004 归因不了
- **滑动窗口模型（本阶段核心结论）**：
  - 6004 = **滑动窗口请求数限流**，错误码 `429/6004/category:quota`，硬阻断（弹窗"消耗积分继续"不适用）
  - **重置时刻 = 容量绑定请求（窗口内第 C−N+1 旧的那条）+ 窗口长 W**，不必是最早一条
  - 窗口长按模型不同：**ds-v4.1-flash = 24h**；**hy3 = 短窗（≈3h 或 4.5h，样本不足未定，下次触发可解）**
  - 铁证：3 个重置锚点 − 24h 与真实 sendPrompt **±5s 精确吻合**（00:37:33↔Elaine 09-11 首用；20:56:45↔Elaine 晚间请求；14:26:22↔Harvey 切入后首用）
  - 同一绑定未滑出前再次触发 6004，重置时刻不变（hy3 今晚 20:32/20:48 两次同 22:59:49 实证）
  - ⚠️ credit_ledger 三主账本同落 14:26:22 边界与滑动窗模型的兼容性**未完全解释**（Harvey 案例已通：首用+24h）
- 新脚本 `scripts/analysis/find_6004_events.py`：扫 6004 事件（模型/触发/重置）、按对话 join、按 auth 备份归因、解窗口长候选；**检测/复盘工具，非命中前预测**

## 决策（为什么这样做）

- **`sessions.user_id` 不可作账号归因**：每次切号 L3 对齐把它改写为当前 uid——归属重建唯一真相源是 auth 备份时间线（与 MEMORY 既定决策一致，本次在 6004 场景再次验证）。
- **日志里的 `32hex/36hex` 前缀 = 每次请求的相关性 ID，不是账号 ID**：同一对话内出现多个不同值即证（`c522dc02` 内 3 个、`a21e5f21` 内 2 个）。任何"按它分组"的结论无效。
- **"固定每日锚点"两度被否**：①credit_ledger 版（三账号 14:26:22）被 6004 归因后 Elaine 双锚点否；②"触顶后滚动 24h"（H2）被"4 次连触同一重置时刻"否。滑动窗+绑定请求语义是唯一同时满足全部 14 个样本的模型。
- **6004 轴与积分轴独立**：`credit_ledger` 只记成功+计费请求，429 不入账，账本无法观测 6004。
- **全网检索（2026-09-12）**：官方《错误码处理说明》对 6004 仅说"切换模型重试"，无机制；腾讯云社区明确"**重置时间由系统按资源情况动态计算**"——与滑动窗模型互证。php.cn"北京 12 点刷新"说法与本地秒级实测矛盾，判定低质不采信。
- 历史决策（统计口径/账本/对齐分层/自动标定不可靠等）见 `docs/PROGRESS.md` 与下文坑位，此处不重复。

## 坑位（别再踩）

1. **统计口径三条公式**（权威：`crates/wb-switch-core/src/modules/token_stats.rs`）：`total = input + output + cacheWrite`（input 已含 cacheRead）；命中率 `= cacheRead / input`；usage 优先级 `message > providerData > 顶层` 且须有 input 字段；一个 JSONL = 一个对话，排除 `subagents/`。
2. **AI 沙箱里 server 起不来**：取数一律走 `examples/dump_stats.exe`，别折腾 HTTP。
3. example 产物 `target/debug/examples/dump_stats.exe`；GNU 三件套（`RUSTFLAGS=-C link-arg=-fuse-ld=lld` + w64devkit 入 PATH），增量约 15s。
4. AI 沙箱读不了宿主 `CodeBuddyExtension/.../auth`（os error 5）→ 放行前台命令。
5. 版本判定只看 `resources/install-manifest.json` 的 `appVersion`；`version` 文件是 Electron 内核版本。
6. 大仓库 git 写操作不进 2 分钟前台窗口（曾两次损坏 `.git`）；`gc.auto=0` 勿改回；关键节点必须 push。
7. 编译前停掉运行中的 `wb-switch.exe` / `wb-switch-rust.exe`。
8. `.cmd` 必须纯 ASCII + CRLF + 开头 `chcp 65001`。
9. 账本聚合先按 (账号,日,模型) 汇总日累计再取 max，别拿逐行比大小。
10. Git Bash 下 `$USERPROFILE` 会被 mangled → 用 Python `os.path.expanduser("~")`。
11. **6004 日志里没有账号字段**；长哈希先做"同实体多值"反证再当主键用（本次教训：请求相关性 ID 被误当账号 ID）。
12. `npx` 在沙箱触发 Program Blacklist（wsl.exe）→ tsc 直接调 `node.exe node_modules/typescript/bin/tsc --noEmit`。

## 下一步

1. **解死 hy3 窗口长（下次 hy3 触发时）**：跑 `scripts/analysis/find_6004_events.py`，取触发时刻 + 全部 hy3 请求序列，在"锚≈3h（11:59:49Z 候选，或有一条未入 sendPrompt 的内部调用）"vs"4.5h（10:29:49Z cdcec156，精确到秒）"间定案。一次触发即够。
2. **`model_daily_limit_check.py` 重窗方案需重新评估**：原"按 14:26:22 固定锚重窗"的依据（固定锚点）已被滑动窗模型推翻。credit_ledger 的 14:26:22 边界在滑动窗语义下 = 当日绑定请求+24h 的巧合或另有机制 → **先解释 credit_ledger 14:26:22 边界与滑动窗的兼容性**，再决定检查脚本是否还需要重窗（`verify_reset_anchor.py` 的"固定锚"结论同步作废待复核）。
3. **find_6004_events.py 可选增强**：自动做"重置−W 与请求序列匹配"的解窗输出（当前手动解）；把 auth 备份归因集成进主输出表。
4. 安装 WorkBuddy **5.5.6**（包在 `C:\Users\WH\AppData\Local\Temp\workbuddy-update-x64\`；装前 `git tag pre-5.5.6` + 快照 `~/.wb-switch/`；装后回归切号主题跟随 + 对齐勾选）。
5. 用量监控产品化待定（手动版 `model_daily_limit_check.py` + `check_daily_limit.cmd` 已可用；自动标定不可靠，只能软信号）；账本 UI 化、按对话统计进 webui 均可选。
6. issue #30 跟进（vite → 账号发现 → 数据对齐 顺序提 PR）；H 余额 95% 已用，切号对话框"余额告急+逼近峰值"提醒可做。
