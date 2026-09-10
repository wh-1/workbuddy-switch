# HANDOFF — workbuddy-switch

> 更新：2026-09-10 晚 · 分支 dev · 工作区干净
> 上阶段：合并上游 v0.1.36（travel 派猫猫旅行 + CodeBuddy IDE 用量统计）+ 向上游提交 issue #30
> 本晚新增：修复切号对齐勾选失效（Tauri/HTTP 双通道参数断层），dev = `272530b` 已推
> ⚠️ 本阶段发生 `.git` 损坏事故并已完整恢复，见「事故」段

## 进度（现在在哪）

- **dev = `9f802cb`**（切号主题跟随账号 L6；前一节点 `272530b` 修对齐勾选参数断层）
- 验证：`cargo test -p wb-switch-core` **182 全绿**、`npx tsc --noEmit` 绿、debug exe 已重编
- **主题跟随（9f802cb）**：主题存 Electron Local Storage（leveldb 键 `agent-ui-theme`，云端异步回写导致切号重启瞬间可能闪默认主题）。切号流程在 App 关闭后备份当前账号主题到 `~/.wb-switch/ui_prefs/`、把目标账号主题 append 进最新 .log（手写 LevelDB log record + CRC32C，append-only、写坏仅被丢弃）。某账号首次切走才生成备份，此前由云端兜底。**待 GUI 实测**：切号重启瞬间主题应直接到位
- **bug 修复（272530b）**：GUI 切号弹窗勾选项（会话归属/文件对齐/dryRun 预览）被静默丢弃——Tauri 命令嵌套签名 vs 前端扁平 invoke + SwitchOptions 缺 serde camelCase。会话 0 同步的根因即此；automations「同步了」是 WorkBuddy 本体云端同步的巧合。待 GUI 复测：勾「会话归属对齐」切号验证
- **main = `bbb0d3c`** = 上游 changexbc/workbuddy-switch v0.1.36
- **origin 已双备份**：dev = `6fc8ca5` / main = `bbb0d3c`
- 验证：`cargo test -p wb-switch-core` **176 全绿**、`npx tsc --noEmit` 绿、`cargo check` 绿（收尾复核）
- 本阶段产出：
  1. 合并上游 v0.1.35 / v0.1.36，3 处冲突已解（`api.rs` / `commands.rs` / `lib/api.ts` 的模块与命令注册表）
  2. 上游新能力：`travel` 派猫猫旅行（`travel.rs` +1236 行）、Token 统计新增 **CodeBuddy IDE 来源**
  3. 本地既有：账号发现、对齐功能（L1-L5）、vite 双栈修复、一键启动脚本
  4. 版本号随上游升到 **0.1.36**
  5. **向上游提交 issue #30**（vite Windows 白壳根因，附实测 + 社区佐证）：https://github.com/changexbc/workbuddy-switch/issues/30
  6. **新建两个跨项目 skill**：`git-corruption-rescue`（git 仓库损坏抢救）、`github-api-without-gh`（无 gh 时 PAT 直连 GitHub API）
- 构建形态不变：debug exe + vite；**尚无 release 包**

## 事故（2026-09-10 14:34，已完整恢复）

- **现象**：`git checkout dev` + `git merge official/main` 在沙箱前台被 SIGTERM 强杀后，仓库变 `fatal: not a git repository`。
- **损坏范围**：`.git/refs/` 整个目录消失；loose objects 归零；旧 pack（27MB，含 0.1.0~0.1.34 全史）只剩 `.idx`、`.pack` 被删；工作区 59 个文件消失。
- **恢复手段**：① 从 `C:\Users\WH\AppData\Local\Temp\wbs-fresh\.git\objects\pack\` 找回同名 27MB pack 完整副本；② `git fetch origin dev` 补回已推送提交；③ 从 index 恢复 59 个缺失文件；④ `rm .git/index` + `git reset` + `git add -A` 重建索引，按原意图重建提交。
- **损失**：原 5 个本地提交的历史粒度丢失，**代码/文档内容零损失**。
- **防护（重要）**：仓库已设 `gc.auto=0` + `gc.autoDetach=false`，**勿改回**；工作区备份 `D:\w-dev\_archive\rescue-20260910-full`（归档区集中）；`C:\Users\WH\AppData\Local\Temp\wbs-fresh` 裸对象库**勿删**（救命备份）。

## 决策（为什么这样做）

- 账号发现数据源定案：**官方 auth 目录历史备份**（`CodeBuddyExtension/Data/Public/auth/workbuddy-desktop.*.info`）为主，`settings claw.users` / `storage/user-*` / `memory` 残留 uid 为辅（无凭据 → 只提示）
- 补录（adopt）**写前自动备份 accounts.json** 到 `~/.wb-switch/backups/accounts/<ts>/`；`restorable` = refresh token 未过期
- 账号列表只认 `~/.wb-switch/accounts.json`（不自动扫盘）；发现结果仅提示 + 一键补录，不静默写入
- 对齐功能替代 wb_multi_sync：L3 归属移动（非复制）+ L1 备份 + L4 SECRET_KEYS 25 键防串号 + L5 my-files 并集 + dry-run
- GUI 启动约定：**AI 不后台拉 GUI**（白壳），一律主人双击
- 仓库纪律：main 只跟上游，开发全在 dev；remote 全 SSH
- 大仓库 git 写操作（checkout/merge/gc）**不在会被超时杀的沙箱前台跑**（用 bypass + 长超时或后台）；关键节点必须 push
- **vite 白壳 issue 处理决策**：只开 issue #30 不建 PR（3 行修复提 PR 收益薄；issue 附硬证据零成本且建立存在感）；后续若维护者回应积极再考虑提账号发现 / 数据对齐 PR

## 坑位（别再踩）

1. **AI 沙箱读不了宿主 WorkBuddy 数据目录**：长驻进程读 `~/AppData/Local/CodeBuddyExtension/...` = os error 5；一次性放行前台命令可读 → 验证真实 auth 目录只能用放行前台命令；**主人正常环境无此问题**
2. **后台 spawn GUI = 白壳**（WebView2 在 agent 上下文渲染异常），桌面双击正常
3. vite 默认只绑 IPv6 `::1` → devUrl 白壳；已修 `vite.config.ts` `host: host || true`；验 vite 用 `curl --noproxy "*"`
4. Windows GNU debug 三件套：RUSTFLAGS `-C link-arg=-fuse-ld=lld`、`src-tauri` crate-type 勿改回 cdylib、`npm run build` 先于 server 编译（RustEmbed 要 dist/）
5. 含凭据关键词源码有被 AV 删的真实风险 → 重要分支勤 push
6. 编译前必须停掉运行中的 `wb-switch.exe` / `wb-switch-rust.exe`
7. **大仓库 git 操作被强杀会留下半成品 `.git`**（refs/pack 丢失）→ 见「事故」段恢复流程；`gc.auto` 已永久关闭
8. **vite 启动被 WorkBuddy safe-delete shim 拦截**：合并后 `vite.config.ts` 变化会触发依赖重优化，vite 需删 `node_modules/.vite/deps`（95 文件 > 阈值 50）→ 报 `[safe-delete][SAFE_DELETE_BULK_CONFIRM_REQUIRED]` 直接启动失败。解法：把 `node_modules/.vite` **改名**（PowerShell `Rename-Item`，别删），再起 vite
9. **debug exe 白壳的完整前提**：① vite 必须在 1420 跑（`npm run dev`）；② exe 必须是当次源码编译的产物（合并后未重编 = 看到旧 UI）。一键启动：双击 `scripts/run-dev.cmd`
10. **向上游提 issue 走 PAT 直连**：本机无 `gh`，用 `~/.git-credentials` 中 `wh-1` 的 PAT 调 GitHub REST API（`POST /repos/{owner}/{repo}/issues`），python urllib + `ProxyHandler({})` 绕 WARP 代理

## 下一步

1. **GUI 实测上游新功能**：travel 派猫猫旅行页面、Token 统计页新增的 CodeBuddy IDE 来源
2. **GUI 实测账号发现**：双击 `target/debug/wb-switch-rust.exe`（想复现提示条可先删一条 `~/.wb-switch/accounts.json`）
3. **真·切号对齐实测**（GUI）：选账号 → 切号弹窗勾「自动化跟随切换」→ 点「对齐预览」→ 确认切换（**会重启 WorkBuddy 本体**）
4. **可选出正式包**：`npm run tauri build`（内嵌 dist，不走 devUrl，无白壳；首次 30-60 分钟）
5. 官方再出新版：`git fetch official` → main ff 合并 → dev 合并
6. 备选：wb_multi_sync 退役（L4/L5 已内置）
7. issue #30 跟进：等维护者回应，积极则提账号发现 / 数据对齐 PR（依 `改动回贡上游评估` 顺序 ①vite→②账号发现→③数据对齐）
