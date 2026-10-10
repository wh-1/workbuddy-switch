// gateway(私有) —— 摘取上游 PR 时整体剔除。
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Badge } from "@/components/ui/badge";
import type { GatewayAccountStatus } from "@/lib/gateway";
import { formatTime } from "@/lib/gateway";

interface Props {
  accounts: GatewayAccountStatus[];
}

/**
 * 恢复时刻文案 —— 上游 `reset_at` 是权威，网关 `until` 只是自家软冷却。
 * 实测两者可差数小时（`until` 18:25 vs `reset_at` 22:08），只显示 `until` 会让人以为到点就能用。
 */
function recoveryText(until?: string, resetAt?: string): { main: string; sub?: string } {
  if (resetAt) {
    const main = `上游恢复于 ${formatTime(resetAt)}`;
    if (until && Math.abs(new Date(until).getTime() - new Date(resetAt).getTime()) > 60_000) {
      return { main, sub: `网关冷却至 ${formatTime(until)}` };
    }
    return { main };
  }
  if (until) return { main: `恢复于 ${formatTime(until)}` };
  return { main: "恢复时刻未知" };
}

export function RateLimitLedger({ accounts }: Props) {
  const rows = accounts.flatMap((a) =>
    (a.rate_limited_models ?? []).map((m) => ({
      account: a.nickname || a.uid,
      model: m.model || "?",
      until: m.until,
      resetAt: m.reset_at,
      reason: m.reason,
    })),
  );

  return (
    <Card>
      <CardHeader className="pb-2">
        <CardTitle className="text-sm">模型级限流台账</CardTitle>
        <CardDescription className="text-xs">
          哪个账号的哪个模型还在 6004 限额里、什么时候恢复——以「上游恢复」为准，到期会自己消失。
        </CardDescription>
      </CardHeader>
      <CardContent>
        {rows.length === 0 ? (
          <p className="text-sm text-muted-foreground">目前没有模型在限流，各账号全模型可用。</p>
        ) : (
          <ul className="space-y-1.5">
            {rows.map((r, i) => {
              const t = recoveryText(r.until, r.resetAt);
              return (
                <li key={`${r.account}-${r.model}-${i}`} className="flex items-center gap-2 text-sm">
                  <Badge variant="outline" className="font-mono text-xs" title={r.reason}>
                    {r.model}
                  </Badge>
                  <span className="text-muted-foreground">{r.account}</span>
                  <span className="ml-auto text-right text-xs tabular-nums text-muted-foreground">
                    <span className="block">{t.main}</span>
                    {t.sub ? <span className="block opacity-70">{t.sub}</span> : null}
                  </span>
                </li>
              );
            })}
          </ul>
        )}
      </CardContent>
    </Card>
  );
}
