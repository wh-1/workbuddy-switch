// gateway(私有) —— 跟随同步状态卡：本地当前号 vs 网关池号一致性 + 一键重同步。
// 摘取上游 PR 时整体剔除。
import { useCallback, useEffect, useState } from "react";
import { Link2, RefreshCw, TriangleAlert } from "lucide-react";

import { Button } from "@/components/ui/button";
import { Card, CardContent } from "@/components/ui/card";
import {
  fetchGatewaySyncStatus,
  resyncGateway,
  type GatewaySyncStatus,
} from "@/lib/gateway-sync";

function formatTs(ts: number | null): string {
  if (!ts) return "-";
  return new Date(ts).toLocaleTimeString();
}

export function GatewaySyncCard() {
  const [status, setStatus] = useState<GatewaySyncStatus | null>(null);
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState<string | null>(null);

  const refresh = useCallback(() => {
    void fetchGatewaySyncStatus()
      .then(setStatus)
      .catch((e) => setNote(String(e)));
  }, []);

  useEffect(() => {
    refresh();
    const timer = setInterval(() => {
      if (!document.hidden) refresh();
    }, 30_000);
    return () => clearInterval(timer);
  }, [refresh]);

  const handleResync = async () => {
    setBusy(true);
    setNote(null);
    try {
      const r = await resyncGateway();
      if (r.ok) {
        setNote("已重新同步当前账号到网关");
      } else {
        setNote(r.error || "同步没成功，稍后再试一次");
      }
    } catch (e) {
      setNote(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
      refresh();
    }
  };

  if (!status) return null;

  // 未启用：不占版面（配置在设置页/连接栏完成）。
  if (!status.enabled) return null;

  const mismatch = !status.uidMatch;
  const pending = status.pendingUid != null;

  return (
    <Card className={mismatch ? "border-destructive/50" : undefined}>
      <CardContent className="flex flex-wrap items-center gap-x-3 gap-y-1.5 py-3 text-sm">
        <Link2 className="size-4 shrink-0 text-muted-foreground" />
        <span className="font-medium">跟随同步</span>
        {mismatch ? (
          <>
            <span className="flex items-center gap-1 text-destructive">
              <TriangleAlert className="size-4" />
              网关池和当前账号对不上（本地 {status.localName ?? "未登录"} · 网关{" "}
              {status.gatewayName ?? "空"})
            </span>
            <Button size="sm" variant="outline" className="h-7 px-2 text-xs" disabled={busy} onClick={() => void handleResync()}>
              <RefreshCw className={busy ? "size-3.5 animate-spin" : "size-3.5"} />
              {busy ? "同步中…" : "重新同步"}
            </Button>
          </>
        ) : (
          <span className="text-muted-foreground">
            网关池已是当前账号（{status.gatewayName ?? status.gatewayUid ?? "-"}），上次同步 {formatTs(status.lastSyncAt)}
            {status.lastSyncReason ? ` · ${status.lastSyncReason}` : ""}
          </span>
        )}
        {pending ? (
          <span className="text-amber-600 dark:text-amber-400">有一次同步没写成，下次切号/刷新会自动补上</span>
        ) : null}
        {note ? <span className="text-xs text-muted-foreground">{note}</span> : null}
      </CardContent>
    </Card>
  );
}
