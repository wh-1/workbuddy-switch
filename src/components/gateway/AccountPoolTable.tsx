// gateway(私有) —— 摘取上游 PR 时整体剔除。
import { useState } from "react";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import type { GatewayAccountStatus, GatewayAdminAction } from "@/lib/gateway";
import { formatCountdown, formatTime } from "@/lib/gateway";

interface Props {
  accounts: GatewayAccountStatus[];
  /** 正在执行运维动作的 uid（按钮转圈、防重复点击）。 */
  busyUid?: string | null;
  onAction?: (uid: string, action: GatewayAdminAction, reason?: string) => Promise<void>;
  /** switch 当前登录账号 uid（跟随模式）：与池条目不一致时该行标红提醒。 */
  localUid?: string | null;
  /** switch 当前登录账号展示名（标红提示用，与账号管理页一致）。 */
  localName?: string | null;
}

function successRate(a: GatewayAccountStatus): string {
  const ok = a.success_count ?? 0;
  const err = a.err_total ?? 0;
  const total = ok + err;
  if (total === 0) return "-";
  return `${Math.round((ok / total) * 100)}%`;
}

function stateBadge(a: GatewayAccountStatus) {
  if (a.manual_disabled) {
    return (
      <Badge
        variant="outline"
        className="bg-purple-500/10 text-purple-600 dark:text-purple-400"
        title={a.manual_reason ? `摘除原因：${a.manual_reason}` : undefined}
      >
        已摘除
      </Badge>
    );
  }
  if (a.disabled) {
    return (
      <Badge variant="outline" className="bg-red-500/10 text-red-600 dark:text-red-400" title={a.disabled_reason}>
        已禁用
      </Badge>
    );
  }
  if (a.breaker_until && new Date(a.breaker_until) > new Date()) {
    return (
      <Badge variant="outline" className="bg-orange-500/10 text-orange-600 dark:text-orange-400">
        熔断
      </Badge>
    );
  }
  if (a.cooling) {
    return (
      <Badge variant="outline" className="bg-amber-500/10 text-amber-600 dark:text-amber-400">
        冷却 {formatCountdown(a.cool_remaining_sec)}
      </Badge>
    );
  }
  return (
    <Badge variant="outline" className="bg-emerald-500/10 text-emerald-600 dark:text-emerald-400">
      可服务
    </Badge>
  );
}

function rowAction(
  a: GatewayAccountStatus,
  busy: boolean,
  pick: (a: GatewayAccountStatus) => void,
) {
  const cls = "h-7 px-2 text-xs";
  // 摘除中 → 恢复接单（enable）；系统禁用 → 复活（revive）；正常 → 临时摘除（弹确认）
  if (a.manual_disabled) {
    return (
      <Button variant="outline" size="sm" className={cls} disabled={busy} onClick={() => pick(a)}>
        {busy ? "处理中…" : "恢复接单"}
      </Button>
    );
  }
  if (a.disabled) {
    return (
      <Button variant="outline" size="sm" className={cls} disabled={busy} onClick={() => pick(a)}>
        {busy ? "处理中…" : "复活"}
      </Button>
    );
  }
  return (
    <Button
      variant="ghost"
      size="sm"
      className={`${cls} text-muted-foreground`}
      disabled={busy}
      onClick={() => pick(a)}
    >
      {busy ? "处理中…" : "临时摘除"}
    </Button>
  );
}

export function AccountPoolTable({ accounts, busyUid, onAction, localUid, localName }: Props) {
  const [disableTarget, setDisableTarget] = useState<GatewayAccountStatus | null>(null);
  const [reason, setReason] = useState("");

  const pick = (a: GatewayAccountStatus) => {
    if (!onAction) return;
    if (a.manual_disabled) {
      void onAction(a.uid, "enable");
      return;
    }
    if (a.disabled) {
      // 系统自动禁用（连续失败等）：enable 解不了它，得 revive。
      void onAction(a.uid, "revive");
      return;
    }
    // 「临时摘除」要先弹确认（带原因输入）；恢复/复活低风险直接执行。
    setReason("");
    setDisableTarget(a);
  };

  const handleConfirmDisable = async () => {
    if (!disableTarget || !onAction) return;
    try {
      await onAction(disableTarget.uid, "disable", reason.trim() || undefined);
    } catch {
      // 错误由页面级 actionError 展示，这里只收窗保留现场
    }
    setDisableTarget(null);
    setReason("");
  };

  const sorted = [...accounts].sort((a, b) => {
    // 停用/禁用的沉底，可服务的按积分降序
    const rank = (x: GatewayAccountStatus) => (x.manual_disabled || x.disabled ? 2 : x.cooling ? 1 : 0);
    if (rank(a) !== rank(b)) return rank(a) - rank(b);
    return (b.credits ?? 0) - (a.credits ?? 0);
  });

  return (
    <div className="space-y-2">
      <div className="rounded-lg border">
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead>账号</TableHead>
              <TableHead>区域</TableHead>
              <TableHead className="text-right">积分</TableHead>
              <TableHead>状态</TableHead>
              <TableHead className="text-right">成功率</TableHead>
              <TableHead className="text-right">在途</TableHead>
              <TableHead>最近成功</TableHead>
              {onAction ? <TableHead className="text-right">操作</TableHead> : null}
            </TableRow>
          </TableHeader>
          <TableBody>
            {sorted.map((a) => {
              // 跟随模式：池里唯一账号应该就是 switch 当前号；不一致说明同步断了。
              const stale = localUid != null && a.uid !== localUid;
              return (
              <TableRow key={a.uid} className={stale ? "bg-destructive/5" : undefined}>
                <TableCell className="max-w-40 truncate font-medium" title={stale ? `池内 ${a.nickname || a.uid} ≠ 当前账号 ${localName || localUid}，同步断了，去跟随同步卡里点「重新同步」` : a.uid}>
                  {a.nickname || a.uid}
                  {stale ? <span className="ml-1.5 text-xs text-destructive">≠ 当前</span> : null}
                </TableCell>
                <TableCell>
                  <span className="text-xs text-muted-foreground">{a.realm || "-"}</span>
                </TableCell>
                <TableCell className="text-right tabular-nums">{(a.credits ?? 0).toLocaleString()}</TableCell>
                <TableCell>{stateBadge(a)}</TableCell>
                <TableCell className="text-right tabular-nums">{successRate(a)}</TableCell>
                <TableCell className="text-right tabular-nums">{a.in_flight ?? 0}</TableCell>
                <TableCell className="text-xs text-muted-foreground">{formatTime(a.last_success)}</TableCell>
                {onAction ? (
                  <TableCell className="text-right">{rowAction(a, busyUid === a.uid, pick)}</TableCell>
                ) : null}
              </TableRow>
              );
            })}
          </TableBody>
        </Table>
      </div>

      <Dialog
        open={disableTarget !== null}
        onOpenChange={(o) => {
          if (!o) {
            setDisableTarget(null);
            setReason("");
          }
        }}
      >
        <DialogContent className="sm:max-w-md">
          <DialogHeader>
            <DialogTitle>临时摘除「{disableTarget?.nickname || disableTarget?.uid}」？</DialogTitle>
            <DialogDescription>
              摘除后它不再接新对话，但签到、保活和积分台账照常跑；想让它回来时点「恢复接单」就行。
            </DialogDescription>
          </DialogHeader>
          <Input
            value={reason}
            onChange={(e) => setReason(e.target.value)}
            placeholder="留个备注，比如：先让它歇两天（可不填）"
            maxLength={200}
            autoFocus
          />
          <DialogFooter>
            <Button
              variant="outline"
              size="sm"
              onClick={() => {
                setDisableTarget(null);
                setReason("");
              }}
            >
              先不了
            </Button>
            <Button size="sm" disabled={busyUid === disableTarget?.uid} onClick={() => void handleConfirmDisable()}>
              {busyUid === disableTarget?.uid ? "处理中…" : "摘除"}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}
