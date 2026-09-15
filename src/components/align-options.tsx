import { Switch } from "@/components/ui/switch";
import type { AlignDataReport } from "@/lib/types";

/** 对齐勾选项（本地新增 UI，上游对话框只保留 import）。 */
export interface AlignOptions {
  alignAutomations: boolean;
  alignSessions: boolean;
  alignFiles: boolean;
  /** 同步项目侧栏：补缺占位 + 多余软删（默认开）。 */
  syncProjects: boolean;
  /** 会话瘦身：每项目保留最近 1 条（默认开）。 */
  slimSessions: boolean;
}

interface Props {
  value: AlignOptions;
  onChange: (next: AlignOptions) => void;
  previewLines: string[] | null;
}

function Row({
  title,
  hint,
  checked,
  onCheckedChange,
}: {
  title: string;
  hint: string;
  checked: boolean;
  onCheckedChange: (v: boolean) => void;
}) {
  return (
    <div className="flex items-center justify-between gap-3 rounded-md border px-3 py-2.5">
      <div className="min-w-0 flex-1">
        <div className="text-sm font-medium">{title}</div>
        <div className="text-xs text-muted-foreground">{hint}</div>
      </div>
      <Switch checked={checked} onCheckedChange={onCheckedChange} />
    </div>
  );
}

/** 切号弹窗里的三个对齐开关 + 预览结果。 */
export function AlignOptionsPanel({ value, onChange, previewLines }: Props) {
  return (
    <>
      <Row
        title="定时任务迁入"
        hint="把当前所有未删除定时任务的归属迁到目标账号，切换后即可见可管（本地操作，不影响云端）"
        checked={value.alignAutomations}
        onCheckedChange={(v) => onChange({ ...value, alignAutomations: v })}
      />

      <Row
        title="设置同步"
        hint="settings 配置、storage 用户数据、画像缓存按最近活跃账号补齐；my-files 取全账号并集；界面主题跟随目标账号（凭据类键不迁移，防串号）"
        checked={value.alignFiles}
        onCheckedChange={(v) => onChange({ ...value, alignFiles: v })}
      />

      <Row
        title="同步项目侧栏"
        hint="切换后侧栏项目与当前账号一致：缺的项目补一个空白占位对话，多余项目下的对话软删（JSONL 保留可恢复）"
        checked={value.syncProjects}
        onCheckedChange={(v) => onChange({ ...value, syncProjects: v })}
      />

      <Row
        title="会话瘦身"
        hint="每个项目只保留最近 1 条对话，其余软删（JSONL 保留，Token 统计不受影响）；云端那份也一起清掉——只动『云端就归这次要切的账号』的对话，别人的不碰；云端没删掉就不动本地，下次切号再来一次"
        checked={value.slimSessions}
        onCheckedChange={(v) => onChange({ ...value, slimSessions: v })}
      />

      {previewLines && (
        <div className="rounded-md border bg-muted/40 px-3 py-2.5">
          <div className="mb-1 text-xs font-medium text-muted-foreground">对齐预览</div>
          <ul className="space-y-0.5 text-xs">
            {previewLines.map((line, i) => (
              <li key={i} className="min-w-0 break-all">
                {line}
              </li>
            ))}
          </ul>
        </div>
      )}
    </>
  );
}

/** 取路径最后一段做项目名（`D:\w-dev\x` → `x`）。 */
function basename(cwd: string): string {
  const norm = cwd.replace(/[\\/]+$/, "");
  const i = Math.max(norm.lastIndexOf("\\"), norm.lastIndexOf("/"));
  return i >= 0 ? norm.slice(i + 1) : norm;
}

/** 把对齐报告渲染成人类可读的行（预览与切换完成提示共用）。 */
export function formatAlignReport(r: AlignDataReport): string[] {
  if (r.error) return [`对齐出错：${r.error}`];
  if (r.noop) return ["没有勾选任何对齐项"];
  const lines: string[] = [];
  if (r.automations) {
    lines.push(`自动化归属：${r.automations.updated} 条待对齐`);
    if (r.automations.outbox) lines.push(`投递队列：${r.automations.outbox} 条`);
  }
  if (r.sessions) {
    lines.push(
      r.sessions.error
        ? `会话归属出错：${r.sessions.error}`
        : `会话归属：${r.sessions.updated} 条待对齐`,
    );
  }
  const f = r.settings;
  if (f) {
    if (f.claw?.changed) lines.push(`settings.json：${f.claw.changed} 项差异`);
    else if (f.claw) lines.push("settings.json：已一致");
    if (f.storage) {
      lines.push(
        `storage 文件：待复制 ${f.storage.copied}，跳过 ${f.storage.skipped}，并集 ${f.storage.deferred}`,
      );
    }
    if (f.memory) lines.push(f.memory.changed ? "画像缓存：待对齐" : "画像缓存：已一致");
    if (f.myFiles) lines.push(`my-files.json：${f.myFiles.files} 份，待更新 ${f.myFiles.changed}`);
    if (f.theme) lines.push("界面主题：跟随目标账号");
  }
  if (r.projects) {
    if (r.projects.error) lines.push(`项目侧栏同步出错：${r.projects.error}`);
    else
      lines.push(
        `项目侧栏：待补 ${r.projects.addedCount ?? 0} 个占位，待删 ${r.projects.removedCount ?? 0} 条多余对话`,
      );
    // 破坏性操作：列出将被删空的项目（最多 3 个）
    const rm = r.projects.removedProjects ?? [];
    if (rm.length) {
      const names = rm
        .slice(0, 3)
        .map((x) => `${basename(x.cwd)}(${x.sessions})`)
        .join("、");
      lines.push(`　将清空：${names}${rm.length > 3 ? ` 等 ${rm.length} 个项目` : ""}`);
    }
  }
  if (r.slim) {
    if (r.slim.error) lines.push(`会话瘦身出错：${r.slim.error}`);
    else lines.push(`会话瘦身：每项目留 ${r.slim.keep ?? 1} 条，待删 ${r.slim.planned ?? 0} 条`);
    const gs = r.slim.groups ?? [];
    if (gs.length) {
      const names = gs
        .slice(0, 3)
        .map((g) => `${basename(g.cwd)}(${g.count})`)
        .join("、");
      lines.push(`　涉及：${names}${gs.length > 3 ? ` 等 ${gs.length} 个项目` : ""}`);
    }
    if (r.dryRun) {
      const cp = r.slim.copyPlanned;
      lines.push(
        cp && cp.total > 0
          ? `　实际执行时本次复制的 ${cp.total} 条会被跳过，其中 ${cp.hitCount} 条落在上述 ${cp.hitProjects} 个瘦身项目，实际删除数更少`
          : "　实际执行时本次复制的对话会被跳过，删除数可能更少",
      );
    }
    const cl = r.slim.cloud;
    if (cl?.enabled) {
      if (!cl.tokenReady) {
        lines.push("　云端这轮先跳过：没读到该账号的登录凭证，只做本地瘦身");
      } else if (r.dryRun) {
        lines.push(`　云端也会一起清：约 ${cl.planned ?? 0} 条（只动云端就归本账号的）`);
      } else {
        const removed = cl.removed ?? 0;
        const gone = cl.alreadyGone ?? 0;
        const total = cl.deleted ?? removed + gone;
        const parts: string[] = [];
        if (total > 0) {
          parts.push(
            gone > 0
              ? `云端清了 ${total} 条（真删 ${removed} 条，另有 ${gone} 条云端本来就没留）`
              : `云端真删掉 ${removed || total} 条`,
          );
        }
        if (cl.noToken) parts.push(`${cl.noToken} 条没读到登录凭证，只在本机收拾了`);
        if (cl.failed) parts.push(`${cl.failed} 条云端没删掉，本机先留着，下次切号再试`);
        if (cl.foreign) parts.push(`${cl.foreign} 条云端归别的账号，没碰`);
        if (parts.length) lines.push(`　${parts.join("；")}`);
        else lines.push("　云端这轮没有要清的");
      }
    }
  }
  lines.push(r.dryRun ? "以上为预览结果，尚未落盘" : "对齐完成（已先备份 db 与 settings）");
  return lines;
}
