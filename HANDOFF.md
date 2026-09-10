# HANDOFF — workbuddy-switch

> 更新：2026-09-10 14:05 · 分支 dev · 工作区干净
> 上阶段：v0.1.34 基线 + 多账号数据全量对齐 + 账号发现功能

## 进度（现在在哪）

- **dev = 17e4d5f**（`93481a9` 账号发现功能 + `17e4d5f` AGENTS 追加段）；`00bdb43` vite 双栈修复
- **main = 75344b4**：与上游 changexbc/workbuddy-switch v0.1.34 一致
- 本阶段产出：
  1. **对齐功能 API 实测通过**（CLI server 57890）：dry-run 预览 9 自动化 + 54 会话（含软删口径）待归属、storage 复制 1 文件、零落盘复核；真实切换待 GUI 实操
  2. **账号发现功能**（新）：扫官方 auth 历史备份识别曾登录账号 + 一键补录
  3. **3 账号全部在册**：H(970a8619) / Elaine(b69dbd0c) / Harvey(9aba8baf，经 adopt 真实补录)
  4. **vite 双栈修复**：host:true，devUrl 白壳根治
- 测试 **159 通过** + tsc 绿；⚠️ 但 **`cargo test` 整体并不绿** —— lib unittests 二进制启动失败（见坑位 7）；**尚无 release 包**（只有 debug exe，前端走 vite）

## 决策（为什么这样做）

- 账号发现数据源定案：**官方 auth 目录历史备份**（`~/AppData/Local/CodeBuddyExtension/Data/Public/auth/workbuddy-desktop.*.info`，每次登录留档含完整 token）为主，`settings claw.users`/`storage/user-*`/`memory` 残留 uid 为辅（无凭据 → 只提示）
- 补录（adopt）**写前自动备份 accounts.json** 到 `~/.wb-switch/backups/accounts/<ts>/`；`restorable` 判定 = refresh token 未过期
- 账号列表只认 `~/.wb-switch/accounts.json`（不自动扫盘）；发现结果仅提示 + 一键补录，不静默写入
- 对齐功能替代 wb_multi_sync：L3 归属移动（非复制）+ L1 备份 + L4 SECRET_KEYS 25 键防串号 + L5 my-files 并集 + dry-run
- GUI 启动约定：**AI 不后台拉 GUI**（白壳），一律主人双击
- 仓库纪律：main 只跟上游，开发全在 dev；remote 全 SSH

## 坑位（别再踩）

1. **AI 沙箱读不了宿主 WorkBuddy 数据目录**：长驻进程 `read_dir ~/AppData/Local/CodeBuddyExtension/...` = os error 5（拒绝访问）；一次性放行前台命令（`dangerouslyDisableSandbox`）可读。→ 验证真实 auth 目录只能用放行前台命令（如 `cargo test ... -- --ignored --nocapture`）；**主人正常环境无此问题**
2. **后台 spawn GUI = 白壳**（WebView2 在 agent 拉起的上下文渲染异常），桌面双击正常 → 见「决策」
3. vite 默认只绑 IPv6 `::1` → devUrl `localhost` 白壳；已修 `vite.config.ts` `host: host || true`；验 vite 用 `curl --noproxy "*"`
4. Windows GNU debug 构建三件套：RUSTFLAGS `-C link-arg=-fuse-ld=lld`（用户变量已持久化）、`src-tauri` crate-type 勿改回 cdylib、`npm run build` 先于 server 编译（RustEmbed 要 dist/）；windows-targets 走 dlltool 首次 2h
5. 含凭据关键词源码有被 AV 删的真实风险（.git 曾损坏）→ 重要分支勤 push
6. 编译前必须停掉运行中的 `wb-switch.exe` / `wb-switch-rust.exe`（锁输出文件 → os error 32/5）
7. **`cargo test` 并非全绿（2026-09-10 外部复核发现）**：能跑的那批 **159 passed / 0 failed**（1.31s），但 `Running unittests src\lib.rs` 的测试二进制**启动即崩** → `0xc0000139 STATUS_ENTRYPOINT_NOT_FOUND`，cargo **退出码非零**，整体判红。即「159 全绿」只覆盖**部分范围**，报「测试全绿」前需看整体退出码。只读线索：该 exe 259MB 为当次新编（非旧残留）；`src-tauri/Cargo.toml` `crate-type = ["rlib"]`（符合坑位 4）；`target/debug/WebView2Loader.dll` 在、而 `target/debug/deps/` 下**无**；`0xc0000139` 语义 = 模块找到但**缺导出** → 疑 WebView2Loader 版本不符或 GNU + lld 链接问题。**排查方向**：`cargo clean -p wb-switch-rust` 重编 → 仍崩则切 `--target x86_64-pc-windows-msvc` 对照

## 下一步

1. **GUI 实测账号发现**：关掉旧 GUI（若在跑）→ `cargo build -p wb-switch-rust`（先确保 vite 在跑或改走 release）→ 双击 `target/debug/wb-switch-rust.exe`；想复现提示条可先删一个账号（`~/.wb-switch/accounts.json` 去掉一条）再看账号页
2. **真·切号对齐实测**（GUI）：选账号 → 切号弹窗勾「自动化跟随切换」→ 点「对齐预览」看报告 → 确认切换（**会重启 WorkBuddy 本体**，切前自动备份 db+settings）
3. **可选出正式包**：`npm run tauri build`（内嵌 dist，不走 devUrl，无白壳；首次 30-60 分钟）
4. 官方出新版时：`git fetch official` → main ff 合并 → dev 合并
5. 备选：wb_multi_sync 退役（L4/L5 已内置）
