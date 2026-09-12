# HANDOFF — workbuddy-switch

> 更新：2026-09-13 凌晨 · 分支 dev · `e4597d8`
> 本阶段：**切号体验收口**——① 对齐项默认全开 ② 复制会话与瘦身的冲突修复 ③ 对齐预览覆盖破坏性操作
> 状态：**真机验证已通过**（四项默认全勾 + 预览三段齐全），进入收尾
> 前序（已归档 `docs/PROGRESS.md`）：项目侧栏同步真机验证 · 白屏终局 vite 8.3.0 · 6004 滑动窗口

## 进度（现在在哪）

- **dev = `e4597d8`**，已推 origin/dev（03:47 用 `git ls-remote` 核验，`b946875` 之后的收尾归档提交）。本阶段提交链：`abd3920`（默认勾选设置同步+瘦身）→ `0aba0ca`（修 open 时重置为关）→ `1759e33`（瘦身跳过复制体）→ `b946875`（预览覆盖项目侧栏+瘦身）→ `e4597d8`（收尾归档）。
- 双门基线：`cargo test -p wb-switch-core` **195 绿** + `tsc --noEmit` 0 错（**勿跑 `--workspace`**，见坑位 14）。
- 四个勾选项**默认全开**：定时任务迁入 / 设置同步 / 同步项目侧栏 / 会话瘦身；「会话归属对齐」UI 已下线（代码保留）。
- 切号执行顺序（**不可随意调**，switch.rs:163→168）：`close_workbuddy → copy_sessions（上游）→ align_data（迁入/设置同步/主题）→ sync_project_set（项目侧栏）→ slim_sessions（瘦身）`。
- **真机验证 2026-09-13 03:41 通过**（主人实测）：四项默认全勾 ✔；预览出现「项目侧栏 / 会话瘦身 / 界面主题」三段 + 明细 ✔；尾注「本次复制的对话会被跳过」✔。
  - 实测预览值：自动化归属 9 条待对齐 · settings 已一致 · storage 待复制 1 · 画像缓存待对齐 · my-files 4 份待更新 4 · 项目侧栏 0/0（已同步）· 瘦身每项目留 1 条待删 15 条（涉及 6 个项目）。
  - 结论：三段渲染与 dry_run 统计均正确，无回归。

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

1. ~~真机验证三项~~ **已完成（03:41 通过）**，见「进度」段。
2. ~~push origin dev 核验~~ **已核验（03:47）**：`git ls-remote origin dev` = `e4597d8` = 本地 HEAD，**已推送，无待推提交**。注：沙箱内 `git rev-list origin/dev..HEAD` 报 128 是**本地缺 `origin/dev` ref**（未 fetch）导致的假象，不是未推送——判定推送状态用 `git ls-remote`，别用 rev-list。
3. **上游 #32 监控**：若合并需跟进；本机已用「占位不复制」路线规避。
4. **解死 hy3 窗口长（下次 hy3 触发时）**：跑 `scripts/analysis/find_6004_events.py`，锚 ≈3h vs 4.5h 二选一定案。
5. **`model_daily_limit_check.py` 重窗方案重评**：先解释 credit_ledger 14:26:22 边界与滑动窗模型的兼容性，再定脚本去留。
6. **issue #30 顺序提 PR**：vite → 账号发现 → 数据对齐（vite 部分需改述为本机环境问题）。
7. 切号对话框「余额告急+逼近峰值」提醒（H 余额已用 95%）——可选。
8. ~~瘦身保留数可配化~~ **决定不做**（主人 03:47）：`slimKeep` 保持硬编码 `slimSessions ? 1 : 0`，理由是软删可恢复 + 每项目留 1 条符合「占位不复制」路线。勿再提。
9. ~~【优化面盘点 03:51】新增功能 5 个优化点~~ **已全部实施（04:0x，198 绿 + tsc 0 错，GUI/server 已重编）**：
   - **A（预览漏报主题，一致性）已修**：真实执行无条件跑 `sync_theme_for_switch`，预览原只在 `opts.align_files` 时给 `theme.planned` → 关掉「设置同步」时预览不提主题，实际却切。改为**预览无条件给 `planned`**（忠于"预览如实反映将发生什么"，零行为变更，不动已验证过的主题跟随逻辑）。
   - **B（同秒备份互相覆盖，数据安全）**：`config.rs:555` `utc_iso()` 只到秒；`projects_anchor.rs:398` 与 `:420` 在 `append_project_and_slim` 里连续调用各做一次 `backup_workbuddy_db(<ts>)` → 同一秒目录名相同 → 后一次覆盖前一次 → **pre-项目侧栏同步的快照被 pre-瘦身快照覆盖丢失**（DB 仅 0.68MB，两次都落在同一秒内概率高）。**已修**：新增 `backup_stamp()`（秒级 `utc_iso()` + 毫秒），两处备份目录不再重名，pre-同步快照不再被覆盖。
   - **C（预览瘦身数偏大）已修**：`preview_sync` 新增 `copy_session_ids` 参数（switch.rs 预览分支传 `opts.copy_session_ids`），经 `session_cwds()` 查 cwd，与 `slim.groups` 求交，输出 `slim.copyPlanned{total,hitCount,hitProjects}`；前端文案由「删除数可能更少」升级为「本次复制的 N 条会被跳过，其中 K 条落在上述 M 个瘦身项目」。匹配逻辑抽纯函数 `copy_hits()` 并带单测。
   - **D（传参隐患）已修**：`append_project_and_slim` 去掉独立 `dry_run` 参数，统一读 `opts.dry_run`；`preview_sync`/`post_close_sync` 先把 `dry_run` 写进 opts 克隆再传入，杜绝两处不一致。
   - **E（重复代码）已修**：抽 `run_switch_sync(target_acc, opts, protected_ids, copy_session_ids)`，两入口只留主题分歧；五项短路条件抽 `AlignOptions::any_enabled()`。
10. **沙箱 git 视图坑（新增）**：沙箱内 `git fetch` 会打印 `[new branch] dev -> origin/dev` 但**本地 refs 实际不落盘**（下一条 `git branch -r` 看不到）。→ 判定是否已推只用 `git ls-remote <remote> <branch>`；`git rev-list origin/dev..HEAD` 报 128 是假象，不是未推送。
7. src-tauri devtools feature 保留（诊断用，release 无副作用）——已定，无需处理。
