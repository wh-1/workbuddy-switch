<!-- TRELLIS:START -->

> 工作目录规范：`D:/w-dev/common/repo-discipline/README.md`（命名 / 目录 / git / 端口 / 安全，19 章）
# Trellis Instructions

These instructions are for AI assistants working in this project.

This project is managed by Trellis. The working knowledge you need lives under `.trellis/`:

- `.trellis/workflow.md` — development phases, when to create tasks, skill routing
- `.trellis/spec/` — package- and layer-scoped coding guidelines (read before writing code in a given layer)
- `.trellis/workspace/` — per-developer journals and session traces
- `.trellis/tasks/` — active and archived tasks (PRDs, research, jsonl context)

If a Trellis command is available on your platform (e.g. `/trellis:finish-work`, `/trellis:continue`), prefer it over manual steps. Not every platform exposes every command.

If you're using Codex or another agent-capable tool, additional project-scoped helpers may live in:
- `.agents/skills/` — reusable Trellis skills
- `.codex/agents/` — optional custom subagents

Managed by Trellis. Edits outside this block are preserved; edits inside may be overwritten by a future `trellis update`.

<!-- TRELLIS:END -->

## 文档地图（本地段，动手前先看一眼）

> 按**变化频率**分档，找东西先按档找，别全盘翻。除 `docs/DEVELOPMENT.md` 外，**下列全部不入库**（含 uid / 昵称 / 会话 id 等隐私）。

| 档 | 落点 | 是什么 | 什么时候用 |
|---|---|---|---|
| 状态 | `.memory/HANDOFF.md` | 进度 / 决策+理由 / 坑位 / 下一步 | **每个新会话先读它**（收尾必写） |
| 历史 | `.memory/PROGRESS.md` | 每阶段一节，只追加不改 | 回溯"什么时候做过什么"（只读尾部约 30 行） |
| 低频参考 | `.memory/references/*.md` · `.memory/archive/` | 从主文件迁出的低频坑 / 已闭环阶段原文 | 主文件指向它时再读 |
| **专题文档** | **`reports/*.md`** | 设计 / 手册 / 验收 / 诊断 / 分析 / 对比（**写一次基本不改**，头部有「类型 ｜ 入库判定」标注） | 做同类任务前找对应专题；接口类先读 `reports/workbuddy-api-handbook-*.md` |
| 脚本与产物 | `scripts/analysis/*.py` · `reports/*.json` | 诊断脚本 + 可重生成的产物 | 需要现算/现扫时跑脚本，别读旧 json 当结论 |

⚠️ **入库判定**：能公开的才进 `docs/`；`reports/`、`.memory/`、`scripts/analysis/` **只进私有主干 `private`**（`.gitignore` 里相关规则已于 2026-09-17 放开 —— 现在这些文件是**入库**的，别照旧注释以为被挡）。发布 fork 前必须由 `publish_to_fork.py` 整体剔除 ⇒ **绝不直接 push fork**。改动脚本输出路径前先看上表 —— 硬编码 `reports/` 的有 28 个文件。

## 规范索引（动手前读什么，3–5 行）

> 只给**指针**，不复制规范正文（复制 = 两处漂移）。没有的写「无」。

| 任务类型 | 该读什么 | 验证命令（能自动判成败） |
|---|---|---|
| 改前端 `src/` | 下方 §UI Component Policy；组件先找 `src/components/ui/` | `node node_modules/typescript/bin/tsc --noEmit`（**勿用 npx**） |
| 改 Rust `crates/` | `.memory/HANDOFF.md` §5 高频坑 + §3 红线（**已证伪的别再投入**） | `cargo test -p wb-switch-core`（**勿 `--workspace`**）；**改了 core 公共字段/命令层还要** `cargo check -p wb-switch-rust`（前者不编 `src-tauri`，漏改会带着编译错误入库） |
| 动云端接口 / 会话 / 切号 | **`reports/workbuddy-api-handbook-*.md`**（8 组接口 / 19 命令 / 12 坑，接口总纲） | **切号后一键复验** `python scripts/analysis/verify_all.py`（四件套：全账枚举 → 本机 → 云端 → 孤儿 → 四分类对账；`--quick` 跳过枚举） |
| 发版 / 构建 | HANDOFF §5 坑 3（**先 vite build 再编 Rust**）、坑 6（exe 硬链接 `LNK1104`） | `npm run build` + `cargo build --release` |
| 写诊断脚本 | `reports/` 同类专题（头部有「类型 ｜ 入库判定」标注） | 脚本自带 dry-run，先看再 apply |

## UI Component Policy

- For frontend UI, prefer the project's existing shadcn components and compose them before writing custom interactive primitives.
- If a required component is missing, add the matching shadcn/Radix component and wrap it under `src/components/ui/` so styling, accessibility, focus management, and behavior stay consistent.
- Write a custom component only when shadcn components and their composition APIs cannot satisfy the requirement. Record the reason before doing so.
- Custom UI must still reuse the project's Rhea theme tokens, spacing, radii, states, and accessibility conventions. Do not substitute native interactive shortcuts such as `details/summary` when an appropriate shadcn component exists.

## Git Commit Language

- Use Conventional Commit type prefixes such as `feat:`, `fix:`, and `docs:`.
- Write the commit subject and body in Chinese by default. Use English only when the user explicitly requests it.

## 阶段交接（上下文纪律）— 本地追加段

> 本段位于 Trellis 托管块（`<!-- TRELLIS:START/END -->`）**之外**，`trellis update` 不会覆盖。
> 本项目 `.memory/HANDOFF.md` 已有自有格式，**沿用即可**；下次收尾时按《`收尾` 通用序列》第 3 步对齐其结构。

- 收到 **`收尾`** = 执行 **`SKILL.md` →《`收尾` 通用序列》（唯一定义处，本文件不复述）**，再加本项目追加项：
  - **追加 1**：改动后跑 `cargo test` + `npx tsc --noEmit`，两者全绿再提交（完整构建另可 `npm run build`）。
  - **追加 2**：commit 遵循本文件「Git Commit Language」节 —— Conventional Commit 前缀 + 中文标题 / 正文。
- 收到 **`读档`**（读盘）= 读 `.memory/HANDOFF.md` → 复述进度 / 下一步 → **等确认，不动手**。
- 收到 **`继续`**（放行）= 确认无误，开始改代码。
- **交接载体是 `.memory/HANDOFF.md`**：`TaskCreate` / `TaskList` 只作**本会话**推进跟踪，**不跨会话保留**，别把"下一步"只落在任务列表里。
- **入库口径（覆盖声明 2026-09-15）**：状态文档被 `.gitignore` 忽略（`HANDOFF.md` / `docs/PROGRESS.md`），**仅本地留存**——开源预备口径，入库判定看目标仓。
- 上下文卫生：命令输出必限流（`| head` / `--limit`），大文件先 Grep 定位再分段读，不在对话里贴大段代码。
- 压缩 ≥2 次 → 立刻收尾换会话。

> 通用规则本体（五条硬规则 / 通用序列 / 四件套规范 / 工具层坑位）在 `~/.workbuddy/skills/context-discipline/SKILL.md`（junction 指向本体仓库，改一处全局生效），**不在本项目复制**。
