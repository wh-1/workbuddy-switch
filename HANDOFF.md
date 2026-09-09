# HANDOFF — workbuddy-switch

> 更新：2026-09-09 19:26 · 分支 dev · 干净无未提交改动（.git-broken/ 已 exclude）

## 进度（现在在哪）

- **dev = e15b9fb**：官方 v0.1.34 基线 + 多账号数据全量对齐功能（157 测试全绿 + tsc 绿）
- **main = 75344b4**：与上游 changexbc/workbuddy-switch v0.1.34 完全一致（已推 wh-1 fork）
- 今日功能已合入官方 0.1.34 修复基线；**尚未出 release 构建**（只有 debug exe，前端走 vite）

## 决策（为什么这样做）

- 对齐功能替代 wb_multi_sync：L3 归属移动（非复制，无双跑）+ L1 备份 + L4 文件一致性（SECRET_KEYS 25 键防串号）+ L5 my-files 并集 + dry-run 预览
- **多账号使用定稿**：自动化本地 owner 对齐随切号（云端查看/管理放弃——手机端是冻结快照，实测四通道全不通）；会话复制到手机锚点账号作桥；对话一律不删
- src-tauri lib crate-type 收敛 `rlib`（Windows GNU debug cdylib 导出超限）；链路工具链 rustup GNU + w64devkit + LLD
- 仓库纪律：main 只跟上游，开发全在 dev；remote 全 SSH（GCM 弹窗已根治）

## 坑位（别再踩）

1. **火绒/Defender 疑似在删本项目含凭据关键词的源文件**（.git refs/objects 曾损坏，已重建）→ **先查隔离区 + 加项目白名单**
2. Windows GNU debug 构建三件套：RUSTFLAGS lld（已持久化用户变量）、crate-type 勿改回 cdylib、`npm run build` 先于 server 编译（RustEmbed 要 dist/）
3. 本机代理对 localhost 探测有假阳性：验 vite 用 `curl --noproxy "*"`；vite 只监听 IPv6 `::1`
4. debug exe 必须先 `npm run dev` 再启动；正式包 `npm run tauri build`（首次全量 30-60 分钟）

## 下一步

1. 【用户】火绒/Defender 隔离区检查 + 项目白名单（最优先）
2. 实测新功能：切号 → 预览对齐 → 确认自动化/会话可见性（debug 版：`npm run dev` + exe）
3. 官方出新版时：`git fetch official` → main ff 合并 → dev rebase/merge
4. 备选：`npm run tauri build` 出正式安装包；给 wb_multi_sync 退役（L4/L5 已内置）
