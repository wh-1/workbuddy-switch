// gateway(私有) —— 账号管理页头部的网关跟随状态小指示（点 = 状态，hover 看详情）。
// 摘取上游 PR 时整体剔除。
import { useCallback, useEffect, useState } from "react";
import { Link2 } from "lucide-react";

import { fetchGatewaySyncStatus, resyncGateway, type GatewaySyncStatus } from "@/lib/gateway-sync";

export function GatewayFollowDot() {
  const [status, setStatus] = useState<GatewaySyncStatus | null>(null);
  const [busy, setBusy] = useState(false);

  const refresh = useCallback(() => {
    void fetchGatewaySyncStatus()
      .then(setStatus)
      .catch(() => setStatus(null));
  }, []);

  useEffect(() => {
    refresh();
    const timer = setInterval(() => {
      if (!document.hidden) refresh();
    }, 30_000);
    return () => clearInterval(timer);
  }, [refresh]);

  if (!status || !status.enabled) {
    // 未启用跟随同步：不占视觉焦点，只有 hover 才能看到的淡图标。
    return (
      <span className="group relative inline-flex cursor-default opacity-40">
        <span className="inline-flex rounded-[22%] bg-muted-foreground/30 p-[6px]">
          <Link2 className="size-4" />
        </span>
        <span className="pointer-events-none absolute right-0 top-full z-50 mt-2 hidden whitespace-nowrap rounded-md bg-popover px-2.5 py-1.5 text-xs text-popover-foreground shadow-lg ring-1 ring-black/5 group-hover:block">
          网关跟随同步未开启（设置里可开启）
        </span>
      </span>
    );
  }

  const mismatch = !status.uidMatch;
  const dotClass = mismatch
    ? "inline-flex rounded-[22%] bg-destructive p-[6px] text-white"
    : "inline-flex rounded-[22%] bg-emerald-600 p-[6px] text-white";

  const detail = mismatch
    ? `网关池和当前账号对不上（本地 ${status.localName ?? "未登录"} · 网关 ${status.gatewayName ?? "空"}），点击重新同步`
    : `网关跟随当前账号（${status.gatewayName ?? "-"}）同步正常`;

  const handleClick = async () => {
    if (!mismatch || busy) return;
    setBusy(true);
    try {
      await resyncGateway();
    } finally {
      setBusy(false);
      refresh();
    }
  };

  return (
    <button
      type="button"
      className="group relative inline-flex cursor-pointer disabled:opacity-60"
      onClick={() => void handleClick()}
      disabled={busy || !mismatch}
      aria-label={detail}
    >
      <span className={dotClass}>
        <Link2 className={busy ? "size-4 animate-spin" : "size-4"} />
      </span>
      <span className="pointer-events-none absolute right-0 top-full z-50 mt-2 hidden whitespace-nowrap rounded-md bg-popover px-2.5 py-1.5 text-xs text-popover-foreground shadow-lg ring-1 ring-black/5 group-hover:block">
        {detail}
        {mismatch ? "（点我重同步）" : ""}
      </span>
    </button>
  );
}
