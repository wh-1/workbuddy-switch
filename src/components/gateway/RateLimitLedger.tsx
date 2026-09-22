// gateway(私有) —— 摘取上游 PR 时整体剔除。
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Badge } from "@/components/ui/badge";
import type { GatewayAccountStatus } from "@/lib/gateway";
import { formatTime } from "@/lib/gateway";

interface Props {
  accounts: GatewayAccountStatus[];
}

export function RateLimitLedger({ accounts }: Props) {
  const rows = accounts.flatMap((a) =>
    (a.rate_limited_models ?? []).map((m) => ({
      account: a.nickname || a.uid,
      model: m.model || "?",
      until: m.until,
    })),
  );

  return (
    <Card>
      <CardHeader className="pb-2">
        <CardTitle className="text-sm">模型级限流台账</CardTitle>
        <CardDescription className="text-xs">
          哪个账号的哪个模型还在 6004 限额里、什么时候恢复——到期会自己消失。
        </CardDescription>
      </CardHeader>
      <CardContent>
        {rows.length === 0 ? (
          <p className="text-sm text-muted-foreground">目前没有模型在限流，各账号全模型可用。</p>
        ) : (
          <ul className="space-y-1.5">
            {rows.map((r, i) => (
              <li key={`${r.account}-${r.model}-${i}`} className="flex items-center gap-2 text-sm">
                <Badge variant="outline" className="font-mono text-xs">
                  {r.model}
                </Badge>
                <span className="text-muted-foreground">{r.account}</span>
                <span className="ml-auto text-xs tabular-nums text-muted-foreground">
                  恢复于 {formatTime(r.until)}
                </span>
              </li>
            ))}
          </ul>
        )}
      </CardContent>
    </Card>
  );
}
