// gateway(私有) —— 摘取上游 PR 时整体剔除。
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { isFuture } from "@/lib/gateway";
import type { GatewayAccountStatus, GatewayStatus } from "@/lib/gateway";

interface Props {
  /** `/status` 顶层权威计数 —— **优先消费它**，前端自算只在后端缺字段时兜底。 */
  status: GatewayStatus;
  /** 兜底自算（积分合计 + 熔断/摘除等顶层没有的细分计数）。 */
  accounts: GatewayAccountStatus[];
}

export function GatewayOverviewCards({ status, accounts }: Props) {
  // 顶层字段是后端权威口径（`healthy` 已排除 disabled/manual/冷却/熔断/降级五个维度）；缺失时才退回本地统计。
  const total = status.total ?? accounts.length;
  const cooling = status.cooling ?? accounts.filter((a) => a.cooling).length;
  const healthy = status.healthy ?? Math.max(0, total - cooling);
  const unavailable = Math.max(0, total - healthy);
  const full = status.in_flight_full ?? 0;
  const inFlight = accounts.reduce((s, a) => s + (a.in_flight ?? 0), 0);
  const credits = accounts.reduce((s, a) => s + (a.credits ?? 0), 0);
  const breaker = accounts.filter((a) => isFuture(a.breaker_until)).length;
  const manual = accounts.filter((a) => a.manual_disabled).length;
  const degrades = accounts.filter((a) => isFuture(a.degrade_until)).length;

  const totalHint = unavailable > 0 ? `${unavailable} 个不可服务` : "全部可服务";
  const healthyHint =
    [cooling > 0 ? `${cooling} 冷却` : "", breaker > 0 ? `${breaker} 熔断` : "", degrades > 0 ? `${degrades} 降级` : "", manual > 0 ? `${manual} 摘除` : ""]
      .filter(Boolean)
      .join(" · ") || "全部可用";
  const realmTotals = status.realm_totals ?? {};
  const activeRealms = Object.keys(realmTotals).filter((r) => (realmTotals[r]?.total ?? 0) > 0);

  const cards: { title: string; value: string; hint: string }[] = [
    { title: "账号总数", value: String(total), hint: totalHint },
    { title: "可服务", value: String(healthy), hint: healthyHint },
    {
      title: "在途请求",
      value: String(inFlight),
      hint: full > 0 ? `${full} 个满载` : inFlight === 0 ? "当前空闲" : "未占满",
    },
    {
      title: "积分合计",
      value: credits.toLocaleString(),
      hint:
        activeRealms.length > 1
          ? activeRealms.map((r) => `${r} ${realmTotals[r]?.total ?? 0}`).join(" · ")
          : "池内账号当前余额",
    },
  ];

  return (
    <div className="grid grid-cols-2 gap-3 lg:grid-cols-4">
      {cards.map((c) => (
        <Card key={c.title}>
          <CardHeader className="pb-1">
            <CardTitle className="text-xs font-medium text-muted-foreground">{c.title}</CardTitle>
          </CardHeader>
          <CardContent className="pt-0">
            <div className="text-2xl font-semibold tabular-nums">{c.value}</div>
            {c.hint ? <div className="mt-0.5 text-xs text-muted-foreground">{c.hint}</div> : null}
          </CardContent>
        </Card>
      ))}
    </div>
  );
}
