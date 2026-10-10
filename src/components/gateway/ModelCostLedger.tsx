// gateway(私有) —— 摘取上游 PR 时整体剔除。
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Badge } from "@/components/ui/badge";
import type { GatewayAccountStatus } from "@/lib/gateway";
import { formatTime } from "@/lib/gateway";

interface Props {
  accounts: GatewayAccountStatus[];
}

/** 单价展示（网关按 `usage.credit` 折算，单位是账号积分、不是货币）：0 = 实测免费。 */
function costText(cost?: number): { text: string; free: boolean } {
  if (cost == null) return { text: "未知", free: false };
  if (cost <= 0) return { text: "免费", free: true };
  return { text: `${cost} / 千 token`, free: false };
}

export function ModelCostLedger({ accounts }: Props) {
  const rows = accounts
    .flatMap((a) =>
      (a.model_costs ?? []).map((c) => ({
        account: a.nickname || a.uid,
        model: c.model || "?",
        cost: c.cost_per_1k,
        samples: c.samples ?? 0,
        lastSeen: c.last_seen,
      })),
    )
    // 收费的排前面（更该留意），同价按样本数降序
    .sort((x, y) => (y.cost ?? 0) - (x.cost ?? 0) || y.samples - x.samples);

  const freeCount = rows.filter((r) => (r.cost ?? 0) <= 0).length;
  const paidCount = rows.filter((r) => (r.cost ?? 0) > 0).length;

  return (
    <Card>
      <CardHeader className="pb-2">
        <CardTitle className="text-sm">模型单价观测</CardTitle>
        <CardDescription className="text-xs">
          网关每成功请求一次就按实际扣费折算一次单价（0 = 免费），观测 6 小时未更新即失效。
          {rows.length > 0
            ? ` 当前 ${freeCount} 个模型免费${paidCount > 0 ? `、${paidCount} 个收费` : ""}。`
            : ""}
        </CardDescription>
      </CardHeader>
      <CardContent>
        {rows.length === 0 ? (
          <p className="text-sm text-muted-foreground">
            还没有单价观测。等网关跑几次对话就有了——这张表用来回答「这个模型花不花钱」。
          </p>
        ) : (
          <ul className="space-y-1.5">
            {rows.map((r, i) => {
              const c = costText(r.cost);
              return (
                <li key={`${r.account}-${r.model}-${i}`} className="flex items-center gap-2 text-sm">
                  <Badge variant="outline" className="font-mono text-xs">
                    {r.model}
                  </Badge>
                  <span className="text-muted-foreground">{r.account}</span>
                  <span className="ml-auto flex items-center gap-2 text-xs tabular-nums text-muted-foreground">
                    <Badge
                      variant="outline"
                      className={
                        c.free ? "bg-emerald-500/10 text-emerald-600 dark:text-emerald-400" : undefined
                      }
                    >
                      {c.text}
                    </Badge>
                    <span>{r.samples} 次样本</span>
                    <span className="opacity-70">{formatTime(r.lastSeen)}</span>
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
