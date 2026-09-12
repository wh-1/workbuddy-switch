import { Switch } from "@/components/ui/switch";
import type { AlignDataReport } from "@/lib/types";

/** 对齐勾选项（本地新增 UI，上游对话框只保留 import）。 */
export interface AlignOptions {
  alignAutomations: boolean;
  alignSessions: boolean;
  alignFiles: boolean;
}

interface Props {
  value: AlignOptions;
  onChange: (next: AlignOptions) => void;
  /** 勾选「会话归属对齐」时取消「复制会话」（二选一） */
  onAlignSessions?: (checked: boolean) => void;
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
export function AlignOptionsPanel({ value, onChange, onAlignSessions, previewLines }: Props) {
  return (
    <>
      <Row
        title="自动化跟随切换"
        hint="把当前所有未删除自动化的归属改到目标账号，切换后目标账号即可见可管（本地操作，不影响云端）"
        checked={value.alignAutomations}
        onCheckedChange={(v) => onChange({ ...value, alignAutomations: v })}
      />

      <Row
        title="会话归属对齐（全量可见）"
        hint="把所有本地会话归属改到目标账号，切换后看到全量对话列表；与上面的复制会话二选一即可"
        checked={value.alignSessions}
        onCheckedChange={(v) => {
          onChange({ ...value, alignSessions: v });
          onAlignSessions?.(v);
        }}
      />

      <Row
        title="数据文件一致性同步"
        hint="settings 配置、storage 用户数据、画像缓存按最近活跃账号补齐；my-files 取全账号并集（凭据类键不迁移，防串号）"
        checked={value.alignFiles}
        onCheckedChange={(v) => onChange({ ...value, alignFiles: v })}
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
  const f = r.files;
  if (f) {
    if (f.settings?.changed) lines.push(`settings.json：${f.settings.changed} 项差异`);
    else if (f.settings) lines.push("settings.json：已一致");
    if (f.storage) {
      lines.push(
        `storage 文件：待复制 ${f.storage.copied}，跳过 ${f.storage.skipped}，并集 ${f.storage.deferred}`,
      );
    }
    if (f.memory) lines.push(f.memory.changed ? "画像缓存：待对齐" : "画像缓存：已一致");
    if (f.myFiles) lines.push(`my-files.json：${f.myFiles.files} 份，待更新 ${f.myFiles.changed}`);
  }
  lines.push(r.dryRun ? "以上为预览结果，尚未落盘" : "对齐完成（已先备份 db 与 settings）");
  return lines;
}
