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
> 本项目 `HANDOFF.md` 已有自有格式，**沿用即可**；下次收尾时按《`收尾` 通用序列》第 3 步对齐其结构。

- 收到 **`收尾`** = 执行 **`SKILL.md` →《`收尾` 通用序列》（唯一定义处，本文件不复述）**，再加本项目追加项：
  - **追加 1**：改动后跑 `cargo test` + `npx tsc --noEmit`，两者全绿再提交（完整构建另可 `npm run build`）。
  - **追加 2**：commit 遵循本文件「Git Commit Language」节 —— Conventional Commit 前缀 + 中文标题 / 正文。
- 收到 **`读档`**（读盘）= 读 `HANDOFF.md` → 复述进度 / 下一步 → **等确认，不动手**。
- 收到 **`继续`**（放行）= 确认无误，开始改代码。
- **交接载体是 `HANDOFF.md`**：`TaskCreate` / `TaskList` 只作**本会话**推进跟踪，**不跨会话保留**，别把"下一步"只落在任务列表里。
- 上下文卫生：命令输出必限流（`| head` / `--limit`），大文件先 Grep 定位再分段读，不在对话里贴大段代码。
- 压缩 ≥2 次 → 立刻收尾换会话。

> 通用规则本体（五条硬规则 / 通用序列 / 四件套规范 / 工具层坑位）在 `C:\Users\WH\.workbuddy\skills\context-discipline\SKILL.md`（junction 指向本体仓库，改一处全局生效），**不在本项目复制**。
