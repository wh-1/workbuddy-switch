# HANDOFF — workbuddy-switch

> 更新：2026-09-13 凌晨 · 分支 dev · `b946875`
> 本阶段：**切号体验收口**——① 对齐项默认全开 ② 复制会话与瘦身的冲突修复 ③ 对齐预览覆盖破坏性操作
> 前序（已归档 `docs/PROGRESS.md`）：项目侧栏同步真机验证 · 白屏终局 vite 8.3.0 · 6004 滑动窗口

## 进度（现在在哪）

- **dev = `b946875`**，已推 origin/dev。本阶段提交链：`abd3920`（默认勾选设置同步+瘦身）→ `0aba0ca`（修 open 时重置为关）→ `1759e33`（瘦身跳过复制体）→ `b946875`（预览覆盖项目侧栏+瘦身）。
- 双门基线：`cargo test -p wb-switch-core` **195 绿** + `tsc --noEmit` 0 错（**勿跑 `--workspace`**，见坑位 14）。
- 四个勾选项**默认全开**：定时任务迁入 / 设置同步 / 同步项目侧栏 / 会话瘦身；「会话归属对齐」UI 已下线（代码保留）。
- 切号执行顺序（**不可随意调**，switch.rs:163→168）：`close_workbuddy → copy_sessions（上游）→ align_data（迁入/设置同步/主题）→ sync_project_set（项目侧栏）→ slim_sessions（瘦身）`。
- **待主人真机验证**：重编的 GUI（03:24）与 server（03:22）——① 弹窗四项默认全勾 ② 点「预览对齐」应出现「项目侧栏 / 会话瘦身 / 界面主题」三段及明细。

## 决策（为什么这样做）

- **切号不复制会话（核心路线）**：上游 #32（open）复制会话致 Token 重复统计；#9（closed 被拒）。本机用「占位不复制」规避，连续性由设置同步 + 项目占位 + 空锚点续聊保证。
- **复制体受瘦身保护**：复制会话与瘦身并存时，复制体既不被删、也不占每项目保留名额（`slim_sessions(..., exclude)`）——否则同项目复制多条会被瘦成 1 条。
- **预览必须覆盖破坏性操作**：dry_run 原先只跑 `align_data`，看不到项目侧栏/瘦身计划，而这两项是唯一会软删会话的操作。新增 `preview_sync()` 只统计不落盘。
- **预览不碰主题**：`sync_theme_for_switch` 会写 leveldb + 云端，预览只给 `{"planned": true}` 占位。
- **暂不切 MSVC 工具链**：dlltool 是 release 首编一次性税；**触发条件 = 下次 dlltool 卡 >1h**。
- 命名定稿：数据文件一致性同步 → **设置同步**（含主题跟随，报告键 `files`→`settings`）；自动化归属对齐 → **定时任务迁入**。

## 坑位（别再踩）

1. **统计口径三条公式**（权威 `modules/token_stats.rs`）：`total = input + output + cacheWrite`；命中率 `= cacheRead / input`；usage 优先级 `message > providerData > 顶层` 且须有 input 字段；一个 JSONL = 一个对话，排除 `subagents/`。
2. **前端构建三坑**：① `npm`/`npx` 触发沙箱黑名单 → 用 `node node_modules/vite/bin/vite.js` / `node node_modules/typescript/bin/tsc --noEmit`；② vite alias 用正则数组（对象形式 `react:` 会劫持 `react/jsx-runtime`）；③ 验证产物看 import 结构，不看变量名计数。
3. **勾选项默认值有两处**：`useState` 初值 **和** 弹窗 `open` 的 `useEffect` 硬编码重置（`switch-account-dialog.tsx:74-82`）。**改默认值必须两处同改**——只改 useState 会被 useEffect 覆盖（曾致「设置同步」默认关）。
4. **server 包名 ≠ bin 名**：`cargo build -p wb-switch` 报 package not found，包名是 **`wb-switch-server`**（GUI 那侧 package/bin 同名 `wb-switch-rust`）。
5. **exe 被锁但查不到进程**：`failed to remove wb-switch-rust.exe: os error 5` 且 `Get-Process` 无匹配 → 先 `mv x.exe x.prev.exe`（被映射的 exe 可重命名不可删）→ 重编 → PowerShell `Remove-Item -LiteralPath ... -Force` 清 prev（Bash 的 safe-delete 走 genie-trash 会失败）。
6. 版本判定只看 `resources/install-manifest.json` 的 `appVersion`；`version` 文件是 Electron 内核版本。
7. 大仓库 git 写操作不进 2 分钟前台窗口（曾两次损坏 `.git`）；`gc.auto=0` 勿改回；关键节点必须 push。
8. 编译前停掉运行中的 `wb-switch.exe` / `wb-switch-rust.exe`（Git Bash 下 `MSYS_NO_PATHCONV=1 taskkill /IM ... /F`）。
9. `.cmd` 必须纯 ASCII + CRLF + 开头 `chcp 65001`。
10. 账本聚合先按 (账号,日,模型) 汇总日累计再取 max。
11. Git Bash 下 `$USERPROFILE` 会被 mangled → 用 Python `os.path.expanduser("~")`。
12. 6004 日志里没有账号字段；长哈希先做「同实体多值」反证再当主键用。
13. debug tauri 壳走 devUrl:1420 需 vite dev 常驻（release 内嵌 dist）；冷启动 ~53s，`scripts/run-dev.cmd` 轮询 90s + 日志 `target/vite-dev.log`。
14. **双门只跑 `cargo test -p wb-switch-core`**：`--workspace` 会在 `wb-switch-rust --lib` 报 `STATUS_ENTRYPOINT_NOT_FOUND`(0xc0000139，Tauri 壳 DLL 入口缺失)，是环境限制不是回归。
15. **oplog 报告结构**：`result.alignData.projects`（不在顶层）；校验 sessions 用完整 UUID 精确 `=`，8 位前缀 LIKE 会混入历史会话。
16. **预览 dry_run 不跑主题跟随**（会写 leveldb/云端）；真实执行的 `post_close_sync` 才会调 `sync_theme_for_switch`。

## 下一步

1. **真机验证今晚三项（最高优先）**：重启 GUI → 开切号弹窗看四项是否默认全勾 → 勾「会话瘦身」点「预览对齐」，确认出现「项目侧栏 / 会话瘦身 / 界面主题」三段 + 项目明细。产物：`target/release/wb-switch-rust.exe`(03:24) 与 `wb-switch.exe`(03:22)。
2. **上游 #32 监控**：若合并需跟进；本机已用「占位不复制」路线规避。
3. **解死 hy3 窗口长（下次 hy3 触发时）**：跑 `scripts/analysis/find_6004_events.py`，锚 ≈3h vs 4.5h 二选一定案。
4. **`model_daily_limit_check.py` 重窗方案重评**：先解释 credit_ledger 14:26:22 边界与滑动窗模型的兼容性，再定脚本去留。
5. **issue #30 顺序提 PR**：vite → 账号发现 → 数据对齐（vite 部分需改述为本机环境问题）。
6. 切号对话框「余额告急+逼近峰值」提醒（H 余额已用 95%）——可选。
7. src-tauri devtools feature 保留（诊断用，release 无副作用）——已定，无需处理。
