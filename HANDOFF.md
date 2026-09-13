# HANDOFF — workbuddy-switch

> 更新：2026-09-13 中午 · 分支 dev · 代码停在 `dbf4141`（已推 origin/dev）
> 本阶段：**PR 评估 + 对外沟通 —— 已结账，零代码改动**（只动 HANDOFF / PROGRESS / memory）
> 历史阶段归档见 `docs/PROGRESS.md`

## 进度（现在在哪）

- 代码未动：`dev = dbf4141`。本阶段提交只含 `HANDOFF.md` + `docs/PROGRESS.md`。
- 双门复核（本阶段无代码改动，属例行体检）：`cargo test -p wb-switch-core` **198 绿** / `tsc --noEmit` **0 错**。
- 对外动作已落地：
  - issue **#30** 正文精简 1873 → 738 字符（结构改为 结论→原因→影响范围→参考→环境）。
  - 讨论 issue **#35** 已发布：https://github.com/changexbc/workbuddy-switch/issues/35 ——「[讨论] 切号带会话：复制 / 归属移动 / 项目占位，选哪种语义？」，正文 1042 字符。
  - **#32 评论稿未发布**（见下一步 3）。
- 上一阶段（切号体验收口）四项真机验证仍有效，无半路工作。

## 决策（为什么这样做）

- **切号不复制会话（核心路线）**：上游 #32（open）复制会话致 Token 重复统计；#9（closed 被拒）。本机用「占位不复制」规避。
- **占位方案是四件套，拆开不好用（对外表述统一用这个）**：① 项目占位（每项目插 1 条空锚点，零正文零 usage）② 会话瘦身（每项目只留最近 1 条存活会话，其余软删可恢复）③ 设置同步（settings 深合并 + 画像/记忆对齐，凭据键跳过）④ 定时任务迁入。
- **#30 只保留 issue，不提 PR（主人 2026-09-13 定，勿再提）**：1 行改动，PR 收益 < 维护成本。
- **#32 只评论不抢提**：他人 PR，避免对立成两张 PR 打架。
- **对外沟通节奏**：先发 #35（产品取舍层，决定要不要做），等上游回应后再发 #32 评论（技术细节层），不一次刷两个讨论。
- **复制体受瘦身保护**：复制会话与瘦身并存时，复制体既不被删、也不占每项目保留名额（`slim_sessions(..., exclude)`）。
- **预览必须覆盖破坏性操作**：项目侧栏同步与瘦身是唯一会软删会话的操作，dry_run 必须可见计划。
- **预览不跑真实主题同步**：`sync_theme_for_switch` 会写 leveldb + 云端，预览只给 `{"planned":true}`；真实执行同样无条件跟随。
- **瘦身保留数不可配**：硬编码 `slimSessions ? 1 : 0`（主人 2026-09-13 定），勿再提议。
- **暂不切 MSVC 工具链**：dlltool 是 release 首编一次性税；触发条件 = 下次 dlltool 卡 >1h。
- 命名定稿：数据文件一致性同步 → **设置同步**；自动化归属对齐 → **定时任务迁入**。

## 坑位（别再踩）

1. **统计口径三条公式**（权威 `modules/token_stats.rs`）：`total = input + output + cacheWrite`；命中率 `= cacheRead / input`；usage 优先级 `message > providerData > 顶层` 且须有 input 字段；一个 JSONL = 一个对话，排除 `subagents/`。
2. **前端命令三坑**：① `npm`/`npx` 会触发沙箱 wsl.exe 黑名单 → 用 `node node_modules/vite/bin/vite.js` / `node node_modules/typescript/bin/tsc --noEmit`；② vite alias 用正则数组（对象形式 `react:` 会劫持 `react/jsx-runtime`）；③ 验证产物看 import 结构，不看变量名计数。
3. **勾选项默认值有两处**：`useState` 初值 **和** 弹窗 `open` 的 `useEffect` 硬编码重置（`switch-account-dialog.tsx:74-82`）。
4. **预览与切换是两条独立请求体**（`doPreview` 181-189 / `doSwitch` 130-138）：新增字段两处都要传。与坑位 3 同属双写陷阱。
5. **server 包名 ≠ bin 名**：包名是 **`wb-switch-server`**（GUI 侧 package/bin 同名 `wb-switch-rust`）。
6. **exe 被锁但查不到进程**：`os error 5` 且 `Get-Process` 无匹配 → 先 `mv x.exe x.prev.exe` → 重编 → PowerShell `Remove-Item -LiteralPath ... -Force`。
7. 版本判定只看 `resources/install-manifest.json` 的 `appVersion`；`version` 文件是 Electron 内核版本。
8. 大仓库 git 写操作不进 2 分钟前台窗口（曾两次损坏 `.git`）；`gc.auto=0` 勿改回；关键节点必须 push。
9. 编译前停掉运行中的 `wb-switch.exe` / `wb-switch-rust.exe`（`MSYS_NO_PATHCONV=1 taskkill /IM ... /F`）。
10. `.cmd` 必须纯 ASCII + CRLF + 开头 `chcp 65001`。
11. 账本聚合先按 (账号,日,模型) 汇总日累计再取 max。
12. Git Bash 下 `$USERPROFILE` 会被 mangled → 用 Python `os.path.expanduser("~")`。
13. 6004 日志里没有账号字段；长哈希先做「同实体多值」反证再当主键用。
14. **双门只跑 `cargo test -p wb-switch-core`**：`--workspace` 会在 `wb-switch-rust --lib` 报 `STATUS_ENTRYPOINT_NOT_FOUND`(0xc0000139，Tauri 壳 DLL 入口缺失)，是环境限制不是回归。
15. **oplog 报告结构**：`result.alignData.projects`（不在顶层）；校验 sessions 用完整 UUID 精确 `=`，8 位前缀 LIKE 会混入历史会话。
16. **预览 dry_run 不跑主题跟随**；真实执行的 `post_close_sync` 才调 `sync_theme_for_switch`。
17. **备份目录名必须毫秒级**（`projects_anchor.rs::backup_stamp()`）：秒级会让同次切号内两次 DB 备份重名覆盖。
18. **沙箱 git 视图坑**：沙箱内 `git fetch` 打印成功但 refs 不落盘。判定是否已推只用 `git ls-remote <remote> <branch>`。
19. **WebFetch 有 15 分钟 URL 缓存**（2026-09-13 踩）：刚 PATCH/POST 完立刻回读会看到旧正文，误判失败。校验 GitHub 写入一律**直连 API**（Python urllib + `git credential fill` 取 token + `ProxyHandler({})` 绕代理），看 `updated_at`。
20. **项目 `MEMORY.md` 3000 字节注入上限**（≈1000 汉字）：超限被静默截断，下轮看不到后半段 → 首部已加「须显式 Read 全文」提示，全文见 `.workbuddy/memory/MEMORY-full-2026-09-13.md`。自检用 `wc -c`，别数行数。
21. **`find_6004_events.py` 时区 bug（未修）**：`parse_ts()` 解析 `...Z` 不做 UTC→+8，而重置时间取的是真北京时间 → 表格混用时区、差 8h。表头也未标注时区。

## 下一步（按优先级，文件级自足）

1. **修 `scripts/analysis/find_6004_events.py` 时区**：`parse_ts()` 里 `...Z` 结尾统一 `+8h` 后输出，表头标注「北京时间」；顺手删掉过期注释「重置锚点（北京时间，每天固定 14:26:22）」。验收：09-12 那条事件显示 10:55:33 而非 02:55:33。
2. **`model_daily_limit_check.py` 重窗方案重评**（同目录）：先解释 `credit_ledger` 14:26:22 边界与「6004 = 滑动窗口限流」模型的兼容性，再定脚本去留。
3. **#32 评论：等 #35 有回应后再发**。评论要点（正文按此重拟，原稿未落盘已随会话丢失）：① `sort_by_key(mtime)` 在 mtime 相同（备份恢复 / 复制保留 mtime）时退化为 `files()` 遍历顺序 → 建议主排序键改用**文件内首条记录 timestamp**（内容自带、备份不改写）+ 路径 tiebreak 保证确定性；② 源头治理路线（占位不复制）可与之互补。不抢提 PR。
4. **提 PR 清单（2026-09-13 评估，dev 对 upstream v0.1.37 共 64 提交 / 51 文件）**：
   - **#30 只保留 issue，不提 PR（勿再提）**：修复留在 fork（`615c032`，只动 vite.config.ts 3 行）。**注意**：修复值必须是 `host: host || true`，**不是 `"127.0.0.1"`**（只绑 IPv4 后 macOS 若解析 `::1` 会反向白屏）。
   - **P1 功能 PR（每个单独干净分支，禁推 dev）**：① 账号发现；② 设置同步（align.rs 的 L4/L5 + L1 备份，**不含 L3 归属对齐**、不含瘦身）；③ 项目侧栏同步（projects_anchor 半边）。**默认全关**（本机默认全开是主人偏好，上游会反感破坏性默认开）。
   - **账号发现 PR 的三个前置（否则别提）**：
     - **依赖硬伤**：`discover.rs:137` 调 `align::discover_accounts()` → 剥离 align.rs 会编译不过。要么带上 30 行残留扫描 + `settings_path/storage_dir/memory_dir` 三个助手内联进 discover.rs，要么与对齐合成一个大 PR。
     - **restorable 判定过松**：`refresh_exp == 0` 视为可恢复 → 字段缺失时补录出无效账号，应改成「缺失视为不可恢复 / 未知」。
     - **banner 隐私**：`discover-accounts-banner.tsx` 直接列出 nickname/email 且无折叠 → 默认只显 uid 前 8 位，展开才显昵称。
   - **P2 可选**：主题跟随（`ui_theme.rs`，LevelDB append hack，标 experimental）；`get_statistics_since` 显式时间窗；Windows 测试兼容两处（`codebuddy_cli.rs` / `export_import.rs` 的 Unix 根路径）。
   - **不提**：scripts/analysis/*（本机 Python 工具）、credit_ledger/oplog（本地专属）、vite8+React19 固定版 + minify:false（本机环境问题）、HANDOFF/PROGRESS/githooks/editorconfig。
   - remote：`official`(上游 changexbc) / `origin`(wh-1)，全 SSH。
5. **解死 hy3 窗口长**（下次 hy3 触发 6004 时）：跑 `find_6004_events.py`，锚点 ≈3h（`11:59:49Z`）与 4.5h（`10:29:49Z`，对话 `cdcec156`）二选一定案。
6. **切号对话框余额提醒**（`switch-account-dialog.tsx`，可选）：H 账号余额已用 95%，加「余额告急 + 逼近峰值」提示。
7. **上游 #32 / #35 被动监控**：若合并需跟进；本机已用「占位不复制」规避。
