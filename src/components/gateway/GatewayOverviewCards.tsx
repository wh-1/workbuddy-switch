// gateway(私有) —— 摘取上游 PR 时整体剔除。
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import type { GatewayAccountStatus } from "@/lib/gateway";

interface Props {
  accounts: GatewayAccountStatus[];
}

export function GatewayOverviewCards({ accounts }: Props) {
  const total = accounts.length;
  const disabled = accounts.filter((a) => a.disabled).length;
  const cooling = accounts.filter((a) => a.cooling && !a.disabled).length;
  const breaker = accounts.filter((a) => (a.breaker_until ? new Date(a.breaker_until) > new Date() : false)).length;
  const inFlight = accounts.reduce((s, a) => s + (a.in_flight ?? 0), 0);
  const credits = accounts.reduce((s, a) => s + (a.credits ?? 0), 0);
  const serving = Math.max(0, total - disabled - cooling);

  const cards: { title: string; value: string; hint?: string }[] = [
    { title: "账号总数", value: String(total), hint: `${disabled} 个已禁用` },
    { title: "可服务", value: String(serving), hint: `${cooling} 个冷却中` },
    { title: "在途请求", value: String(inFlight), hint: breaker > 0 ? `${breaker} 个熔断中` : "无熔断" },
    { title: "积分合计", value: credits.toLocaleString(), hint: "池内账号当前余额" },
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
