# HANDOFF — workbuddy-switch

> 更新：2026-09-13 凌晨 · 分支 dev · `25ab168`
> 本阶段：**① 项目侧栏同步 + 会话瘦身落地**（切号不复制会话，占位锚点替代上游复制路线）
> ② **白屏终局破案**——vite 8.3.0 rolldown 内核根治 rollup 双 React 实例，主人真机确认修复
> 6004 滑动窗口结论已归档 `docs/PROGRESS.md`（2026-09-12 节）

## 进度（现在在哪）

- **dev = `25ab168`**，已推 origin/dev。本阶段提交链：`b67e8bd`（对齐排除软删）→ `115daef`（项目侧栏同步+会话瘦身+命名改造）→ `8c46a68`（run-dev 轮询）→ `c1884fb`（关 minify）→ `49182b1`（vite 8.3.0 终局修复）→ `25ab168`（PROGRESS 归档）。
- 双门基线：cargo 194 测试绿 + tsc 0 错；release 双 exe（wb-switch / wb-switch-rust）主人真机验证通过。
- **项目侧栏同步（`crates/wb-switch-core/src/modules/projects_anchor.rs`，本地专属零冲突）**：
  - `sync_project_set_in_db`：切号后以上个账号项目清单（cwd 集合）为准——少的补（INSERT 占位会话 + 空 JSONL）、多的删（软删）；快照 `~/.wb-switch/project_set_snapshot.json` 级联防护（清单清零/骤减 30% 中断，force 放行）。
  - `slim_sessions_in_db`：每 cwd 保留 updated_at 最新 N 条（soft delete）。
  - `workspace_dir_name`：盘符小写 + `:` 删 + `\`/`/` → `-`（33 目录反推验证）。
  - 前端勾选项「同步项目侧栏」+ 瘦身条数；SwitchOptions/AlignOptions 加 `sync_projects`/`slim_keep`。
- **命名定稿（主人指定）**：数据文件一致性同步 → **设置同步**（含主题跟随，报告键 `files`→`settings`）；自动化归属对齐 → **定时任务迁入**；会话归属对齐 UI 下线（代码保留，默认排除软删）。
- **白屏终局**：rollup（vite 7.3.6）在 Windows 对 react CJS 包生成双模块实例（`react_production`/`react_production$1`），部分组件绑无 dispatcher 副本 → hooks null 白屏。**vite 8.3.0（rolldown 内核）是真解**；minify:false（esbuild 0.28.2 也破坏 React 19）+ react 正则 alias + strictRequires 为配套防御。tauri 加 devtools feature。

## 决策（为什么这样做）

- **切号不复制会话（核心路线）**：上游 #32（open）复制会话致 Token 重复统计（作者本机 67% usage 重复）；#9（closed 被拒）复制去重被 maintainer 否。本路线规避两者：不复制正文 → 无重复 usage、无侧栏副本雪球；连续性由「设置同步 + 项目占位 + 空锚点续聊」保证。
- **会话归属对齐排除软删**：已删对话不参与新对话上下文，改归属无意义（`deleted_at IS NULL`）。
- **L4 设置同步未改**：与项目侧栏管辖域无交集（L4 管 claw.users/SECRET_KEYS/storage）。
- **暂不切 MSVC 工具链**（主人确认）：dlltool 是 release 首编一次性税，切链成本 > 留 GNU；**触发条件 = 下次 dlltool 卡 >1h**。
- 历史决策（统计口径/账本/6004 滑动窗口/对齐分层等）见 `docs/PROGRESS.md` 与下文坑位，此处不重复。

## 坑位（别再踩）

1. **统计口径三条公式**（权威：`crates/wb-switch-core/src/modules/token_stats.rs`）：`total = input + output + cacheWrite`（input 已含 cacheRead）；命中率 `= cacheRead / input`；usage 优先级 `message > providerData > 顶层` 且须有 input 字段；一个 JSONL = 一个对话，排除 `subagents/`。
2. **前端构建三坑**：① `npm`/`npx` 在沙箱触发 Program Blacklist（wsl.exe）→ 一律 `node node_modules/vite/bin/vite.js` / `node node_modules/typescript/bin/tsc --noEmit`；② vite alias 对象形式 `react:` 前缀劫持 `react/jsx-runtime` → 用正则数组；③ **验证产物看 import 结构，不看变量名计数**（minify 改名会骗过 `var react_production` 计数，曾致 alias 修复假阳性）。
3. **AI 沙箱里 server 起不来**：取数走 `examples/dump_stats.exe`。example 产物 `target/debug/examples/dump_stats.exe`；GNU 三件套（`RUSTFLAGS=-C link-arg=-fuse-ld=lld` + w64devkit 入 PATH），增量约 15s。
4. AI 沙箱读不了宿主 `CodeBuddyExtension/.../auth`（os error 5）→ 放行前台命令。
5. 版本判定只看 `resources/install-manifest.json` 的 `appVersion`；`version` 文件是 Electron 内核版本。
6. 大仓库 git 写操作不进 2 分钟前台窗口（曾两次损坏 `.git`）；`gc.auto=0` 勿改回；关键节点必须 push。
7. 编译前停掉运行中的 `wb-switch.exe` / `wb-switch-rust.exe`（os error 5 锁文件；Git Bash 下用 `MSYS_NO_PATHCONV=1 taskkill /IM ... /F`）。
8. `.cmd` 必须纯 ASCII + CRLF + 开头 `chcp 65001`。
9. 账本聚合先按 (账号,日,模型) 汇总日累计再取 max，别拿逐行比大小。
10. Git Bash 下 `$USERPROFILE` 会被 mangled → 用 Python `os.path.expanduser("~")`。
11. **6004 日志里没有账号字段**；长哈希先做"同实体多值"反证再当主键用。
12. **debug tauri 壳走 devUrl:1420 需 vite dev 常驻**（release 才内嵌 dist）；vite 冷启动 ~53s，`scripts/run-dev.cmd` v2 已改轮询 90s + 日志落 `target/vite-dev.log`。
13. WebView2 缓存目录 `%LOCALAPPDATA%\com.wbswitch.app\EBWebView`——清缓存不能解代码问题（排查时可排除但别指望它修）。

## 下一步

1. ~~**主人真机验证完整切号流程（最高优先）**~~ ✅ **已通过（2026-09-13 01:42 实切 Elaine→廿七）**：补 18 占位会话（DB 18/18 归属/标题/空 JSONL 全对）、续聊实证 = 占位锚点 a42ac423 被点开续聊（WB 首条消息自动重命名标题）、快照防护正常、零异常。详见 `.workbuddy/memory/2026-09-13.md`。
2. **上游 #32 监控**：若合并需跟进；本机已用「占位不复制」路线规避。
3. **解死 hy3 窗口长（下次 hy3 触发时）**：跑 `scripts/analysis/find_6004_events.py`，锚 ≈3h vs 4.5h 二选一定案。
4. **`model_daily_limit_check.py` 重窗方案重评**：先解释 credit_ledger 14:26:22 边界与滑动窗模型的兼容性，再定检查脚本去留。
5. ~~安装 WorkBuddy **5.5.6**~~ ✅ **已装（2026-09-12，实测 `resources/install-manifest.json` appVersion=5.5.6）**。装后回归：**01:42 实切验证即在 5.5.6 环境完成** → 项目侧栏同步 ✓ / 设置同步 ✓ / 定时任务迁入 9 条 ✓；**仅剩主题跟随（L6）待主人肉眼确认**。打点：`pre-5.5.6` tag = `0429bb3`（注：标签是装后补打的，语义应为「5.5.6 回归基线」）+ `~/.wb-switch/backups/pre-5.5.6-snapshot/` 4 文件快照。
6. issue #30 跟进（vite → 账号发现 → 数据对齐 顺序提 PR，注意 vite 部分需改述为本机环境问题）；H 余额 95% 已用，切号对话框"余额告急+逼近峰值"提醒可做。
7. src-tauri devtools feature 保留（诊断用，release 无副作用）——已定，无需处理。
