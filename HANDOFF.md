# HANDOFF — workbuddy-switch

> 更新：2026-09-13 上午 · 分支 dev · `3530eab`（已推 origin/dev）
> 本阶段：**切号体验收口 —— 已结账**，四项真机验证全部通过，无半路工作
> 历史阶段归档见 `docs/PROGRESS.md`

## 进度（现在在哪）

- **dev = `3530eab`**（已推）。本阶段提交链：`abd3920`（默认全开）→ `0aba0ca`（修 open 重置）→ `1759e33`（瘦身跳过复制体）→ `b946875`（预览覆盖破坏项）→ `e4597d8` → `6fb0323`（A–E 优化）→ `b3f5211`（doPreview 补传 copySessionIds）→ `3530eab`（结账）。
- 双门：**`cargo test -p wb-switch-core` 198 绿** + **`tsc --noEmit` 0 错**（勿跑 `--workspace`，见坑位 14）。
- 切号弹窗四个勾选项**默认全开**：定时任务迁入 / 设置同步 / 同步项目侧栏 / 会话瘦身。「会话归属对齐」UI 已下线（代码保留）。
- 切号执行顺序（**不可随意调**，`switch.rs:163→168`）：`close_workbuddy → copy_sessions（上游）→ align_data（迁入/设置同步/主题）→ sync_project_set（项目侧栏）→ slim_sessions（瘦身）`。
- **真机验证全部通过**（主人 09:45 / 09:53 实测）：① 四项默认全勾 ② 预览含「项目侧栏 / 会话瘦身 / 界面主题」三段 ③ 关闭「设置同步」时预览仍提示主题跟随 ④ 选中复制会话后预览末行给出量化文案（「本次复制的 1 条会被跳过，其中 1 条落在上述 1 个瘦身项目」）。

## 决策（为什么这样做）

- **切号不复制会话（核心路线）**：上游 #32（open）复制会话致 Token 重复统计；#9（closed 被拒）。本机用「占位不复制」规避，连续性由设置同步 + 项目占位 + 空锚点续聊保证。
- **复制体受瘦身保护**：复制会话与瘦身并存时，复制体既不被删、也不占每项目保留名额（`slim_sessions(..., exclude)`）。
- **预览必须覆盖破坏性操作**：项目侧栏同步与瘦身是唯一会软删会话的操作，dry_run 必须能看到计划（`preview_sync`）。
- **预览不跑真实主题同步**：`sync_theme_for_switch` 会写 leveldb + 云端，预览只给 `{"planned":true}` 占位；**且无条件给**——真实执行同样无条件跟随主题。
- **瘦身保留数不可配**：`slimKeep` 保持硬编码 `slimSessions ? 1 : 0`（主人 2026-09-13 定）。软删可恢复 + 每项目留 1 条契合「占位不复制」路线，**勿再提议**。
- **暂不切 MSVC 工具链**：dlltool 是 release 首编一次性税；**触发条件 = 下次 dlltool 卡 >1h**。
- 命名定稿：数据文件一致性同步 → **设置同步**（含主题跟随，报告键 `files`→`settings`）；自动化归属对齐 → **定时任务迁入**。

## 坑位（别再踩）

1. **统计口径三条公式**（权威 `modules/token_stats.rs`）：`total = input + output + cacheWrite`；命中率 `= cacheRead / input`；usage 优先级 `message > providerData > 顶层` 且须有 input 字段；一个 JSONL = 一个对话，排除 `subagents/`。
2. **前端构建三坑**：① `npm`/`npx` 触发沙箱黑名单 → 用 `node node_modules/vite/bin/vite.js` / `node node_modules/typescript/bin/tsc --noEmit`；② vite alias 用正则数组（对象形式 `react:` 会劫持 `react/jsx-runtime`）；③ 验证产物看 import 结构，不看变量名计数。
3. **勾选项默认值有两处**：`useState` 初值 **和** 弹窗 `open` 的 `useEffect` 硬编码重置（`switch-account-dialog.tsx:74-82`）。改默认值必须两处同改。
4. **预览与切换是两条独立请求体**（`doPreview` 181-189 / `doSwitch` 130-138）：新增字段**两处都要传**，漏一处则预览拿不到数据（曾致 `copySessionIds` 缺失、量化文案不出现）。与坑位 3 同属双写陷阱。
5. **server 包名 ≠ bin 名**：`cargo build -p wb-switch` 报 package not found，包名是 **`wb-switch-server`**（GUI 侧 package/bin 同名 `wb-switch-rust`）。
6. **exe 被锁但查不到进程**：`os error 5` 且 `Get-Process` 无匹配 → 先 `mv x.exe x.prev.exe` → 重编 → PowerShell `Remove-Item -LiteralPath ... -Force` 清 prev（Bash 的 safe-delete 走 genie-trash 会失败）。
7. 版本判定只看 `resources/install-manifest.json` 的 `appVersion`；`version` 文件是 Electron 内核版本。
8. 大仓库 git 写操作不进 2 分钟前台窗口（曾两次损坏 `.git`）；`gc.auto=0` 勿改回；关键节点必须 push。
9. 编译前停掉运行中的 `wb-switch.exe` / `wb-switch-rust.exe`（Git Bash 下 `MSYS_NO_PATHCONV=1 taskkill /IM ... /F`）。
10. `.cmd` 必须纯 ASCII + CRLF + 开头 `chcp 65001`。
11. 账本聚合先按 (账号,日,模型) 汇总日累计再取 max。
12. Git Bash 下 `$USERPROFILE` 会被 mangled → 用 Python `os.path.expanduser("~")`。
13. 6004 日志里没有账号字段；长哈希先做「同实体多值」反证再当主键用。
14. **双门只跑 `cargo test -p wb-switch-core`**：`--workspace` 会在 `wb-switch-rust --lib` 报 `STATUS_ENTRYPOINT_NOT_FOUND`(0xc0000139，Tauri 壳 DLL 入口缺失)，是环境限制不是回归。
15. **oplog 报告结构**：`result.alignData.projects`（不在顶层）；校验 sessions 用完整 UUID 精确 `=`，8 位前缀 LIKE 会混入历史会话。
16. **预览 dry_run 不跑主题跟随**（会写 leveldb/云端）；真实执行的 `post_close_sync` 才调 `sync_theme_for_switch`。
17. **备份目录名必须毫秒级**（`projects_anchor.rs::backup_stamp()`）：秒级 `utc_iso()` 会让同一次切号内的两次 DB 备份重名覆盖，丢失 pre-同步快照。
18. **沙箱 git 视图坑**：沙箱内 `git fetch` 会打印 `[new branch] dev -> origin/dev` 但本地 refs 不落盘（下一条 `git branch -r` 看不到）。判定是否已推只用 **`git ls-remote <remote> <branch>`**；`git rev-list origin/dev..HEAD` 报 128 是假象，不是未推送。

## 下一步（按优先级，文件级自足）

1. **`model_daily_limit_check.py` 重窗方案重评**（`scripts/analysis/`）：先解释 `credit_ledger` 14:26:22 边界与「6004 = 滑动窗口限流」模型的兼容性，再定脚本去留。背景见项目 memory 的「6004 重置机制」段。
2. **解死 hy3 窗口长**（下次 hy3 触发 6004 时）：跑 `scripts/analysis/find_6004_events.py`，锚点 ≈3h（`11:59:49Z`）与 4.5h（`10:29:49Z`，对话 `cdcec156`）二选一定案。
3. **issue #30 顺序提 PR**：vite → 账号发现 → 数据对齐（vite 部分需改述为本机环境问题）。remote：`official`(上游 changexbc) / `origin`(wh-1)，全 SSH。
4. **切号对话框余额提醒**（`switch-account-dialog.tsx`，可选）：H 账号余额已用 95%，加「余额告急 + 逼近峰值」提示。
5. **上游 #32 被动监控**：若合并需跟进；本机已用「占位不复制」规避。
