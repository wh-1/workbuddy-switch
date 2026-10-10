// gateway(私有) —— 网关接口卡：一张卡讲清「网关本体 + 三条协议接口」。
//
// 2026-09-23 由原「本机服务」+「协议端点」两张卡合并：协议端点内嵌后服务清单只剩网关一项，
// 两卡并列既冗余（一张卡描述一个进程），又让「总开关」在卡间撞名 —— 网关启停 vs 扩展协议总闸。
// 合并后按「接口」维度并列：进程是第一条（网关本体），协议是后三条。
//
// 三条协议接口**不是兄弟关系**：`/v1/messages` 与 `/v1/responses` 拿到请求后回环调本机的
// `/v1/chat/completions`（2api `internal/protocol/mount.go` 的 selfBase）。所以 Chat 行标「常开」、
// 不给开关 —— 给了就会产生「关掉 Chat 却开着 Messages」这种必然报错的状态。
//
// 数据源：进程体检 = core `gateway_services`（页面统一轮询后传入）；协议端点 = core `gateway_protocol`。
// 摘取上游 PR 时整体剔除。
import { type ReactNode, useCallback, useEffect, useRef, useState } from "react";
import { Network, Play, Power, RefreshCw } from "lucide-react";

import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Switch } from "@/components/ui/switch";
import {
  fetchGatewayProtocolStatus,
  restartGatewayService,
  saveGatewayProtocolGates,
  type GatewayProtocolEndpoint,
  type GatewayProtocolStatus,
} from "@/lib/gateway-protocol";
import {
  saveGatewayServicesConfig,
  startGatewayService,
  stopGatewayService,
  type GatewayServiceHealth,
  type GatewayServicesStatus,
} from "@/lib/gateway-services";

interface Props {
  /** 页面统一轮询来的进程体检快照；null = 还没拿到。 */
  services: GatewayServicesStatus | null;
  /** 动过进程之后重新体检一次（状态不在本组件里存）。 */
  onRefreshServices: () => void;
}

/** 可写的三个开关（派生字段不进草稿）。 */
type Gates = { enabled: boolean; anthropic: boolean; responses: boolean };

const GATEWAY_ID = "2api";

const DOT: Record<GatewayServiceHealth["health"], string> = {
  ok: "bg-emerald-500",
  degraded: "bg-amber-500",
  down: "bg-muted-foreground/40",
};

function draftOf(s: GatewayProtocolStatus): Gates {
  return { enabled: s.gates.enabled, anthropic: s.gates.anthropic, responses: s.gates.responses };
}

function same(a: Gates | null, b: Gates | null): boolean {
  if (a == null || b == null) return false;
  return a.enabled === b.enabled && a.anthropic === b.anthropic && a.responses === b.responses;
}

/** 一行结论：先把「为什么不能用」说清楚，再指下一步。 */
function verdict(ep: GatewayProtocolEndpoint, on: boolean, master: boolean): string {
  if (!master) return "总闸关着，这条没挂";
  if (!on) return "关着呢，不挂这条";
  if (ep.mounted) return "已挂上";
  return "配着但没探到 —— 存一下让它重启网关";
}

function dotClass(ep: GatewayProtocolEndpoint, on: boolean, master: boolean): string {
  if (!master || !on) return "bg-muted-foreground/40";
  return ep.mounted ? "bg-emerald-500" : "bg-amber-500";
}

/** 进程一句话状态：写明怎么了 + 顺带指下一步，不写「未知错误」这种空话。 */
function describeService(s: GatewayServiceHealth): string {
  if (s.health === "ok") return `在线${s.latencyMs > 0 ? ` · ${s.latencyMs}ms` : ""}`;
  if (s.health === "degraded") {
    if (s.kind && !s.kindMatch) return `端口有人占着，但不是它（${s.kind}）`;
    return s.message ?? "端口开着，但健康检查没过";
  }
  if (!s.exeExists && s.exe) return "没找到程序，先把下面的目录指对";
  return "还没起来";
}

/** 一行接口的壳子：圆点 + 左区（名称/路径/说明）+ 右区（控件）。 */
function Row({ dot, right, children }: { dot: string; right?: ReactNode; children: ReactNode }) {
  return (
    <div className="flex flex-wrap items-center gap-x-3 gap-y-1 rounded-md border px-2.5 py-2 text-sm">
      <span className={`size-2 shrink-0 rounded-full ${dot}`} />
      {children}
      {right ? <span className="ml-auto shrink-0">{right}</span> : null}
    </div>
  );
}

export function GatewayInterfaceCard({ services, onRefreshServices }: Props) {
  const [status, setStatus] = useState<GatewayProtocolStatus | null>(null);
  const [draft, setDraft] = useState<Gates | null>(null);
  /** 上一次「状态同步进草稿」的值：草稿与它相同说明用户没改过，可以跟着新状态走。 */
  const synced = useRef<Gates | null>(null);
  const [busy, setBusy] = useState(false);
  const [svcBusy, setSvcBusy] = useState(false);
  const [note, setNote] = useState<string | null>(null);
  const [svcNote, setSvcNote] = useState<string | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const [rootDraft, setRootDraft] = useState<string | null>(null);

  const apply = useCallback((s: GatewayProtocolStatus) => {
    const base = draftOf(s);
    setStatus(s);
    setDraft((d) => (d == null || same(d, synced.current) ? base : d));
    synced.current = base;
  }, []);

  const refresh = useCallback(() => {
    void fetchGatewayProtocolStatus()
      .then(apply)
      .catch((e) => setErr(e instanceof Error ? e.message : String(e)));
  }, [apply]);

  useEffect(() => {
    refresh();
    const timer = setInterval(() => {
      if (!document.hidden) refresh();
    }, 15_000);
    return () => clearInterval(timer);
  }, [refresh]);

  const gw = services?.services.find((s) => s.id === GATEWAY_ID) ?? null;

  const handleServiceAct = async () => {
    if (!gw) return;
    const running = gw.health !== "down";
    setSvcBusy(true);
    setSvcNote(null);
    try {
      const r = await (running ? stopGatewayService(gw.id) : startGatewayService(gw.id));
      const failed =
        r.action === "failed" || r.action === "stop_not_confirmed" || r.action === "started_not_confirmed";
      setSvcNote(r.message || r.error || (failed ? "没成，稍后再试一次" : "搞定了"));
      // 进程动了，端点挂载也会跟着变，两边都重新拉一次
      refresh();
    } catch (e) {
      setSvcNote(e instanceof Error ? e.message : String(e));
    } finally {
      setSvcBusy(false);
      onRefreshServices();
    }
  };

  const handleRefreshAll = () => {
    onRefreshServices();
    refresh();
  };

  const handleSaveRoot = async () => {
    if (rootDraft == null || !services) return;
    setSvcNote(null);
    try {
      await saveGatewayServicesConfig({ root: rootDraft, services: {} });
      setSvcNote("目录记下了");
      setRootDraft(null);
      onRefreshServices();
    } catch (e) {
      setSvcNote(e instanceof Error ? e.message : String(e));
    }
  };

  const handleSaveGates = async () => {
    if (!draft) return;
    setBusy(true);
    setErr(null);
    setNote(null);
    try {
      const res = await saveGatewayProtocolGates(draft);
      if (!res.changed) {
        setNote(res.message);
        return;
      }
      // 配置改了就一定要重启 —— 端点开关只在网关启动时读一次
      setNote("开关写进配置了，正在重启网关…");
      const rr = await restartGatewayService(GATEWAY_ID);
      const after = await fetchGatewayProtocolStatus();
      apply(after);
      onRefreshServices();
      const mounted = after.endpoints.filter((e) => e.mounted).length;
      setNote(
        rr.action === "restarted"
          ? `网关重启完了，${mounted}/${after.endpoints.length} 条扩展接口在挂`
          : (rr.message ?? "网关重启后端口还没起来，稍等一下再刷新"),
      );
    } catch (e) {
      setErr(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const running = gw != null && gw.health !== "down";
  const master = draft?.enabled ?? false;
  const dirty = draft != null && status != null && !same(draft, draftOf(status));
  const mountedExt = status?.endpoints.filter((e) => e.mounted).length ?? 0;
  const totalIfaces = (status?.endpoints.length ?? 0) + 1;

  const summary =
    services == null
      ? "体检中…"
      : gw == null
        ? "没找到网关服务"
        : gw.health === "down"
          ? "网关没起"
          : `在线 · ${1 + mountedExt}/${totalIfaces} 条接口在挂`;

  return (
    <Card>
      <CardHeader className="pb-2">
        <div className="flex flex-wrap items-center justify-between gap-2">
          <CardTitle className="flex items-center gap-1.5 text-sm">
            <Network className="size-4" />
            网关接口
            <span className="text-xs font-normal text-muted-foreground">{summary}</span>
          </CardTitle>
          <Button
            size="sm"
            variant="ghost"
            className="h-7 px-2 text-xs"
            onClick={handleRefreshAll}
            title="重新体检：进程与端点各探一次，只看端口和身份，不消耗额度"
          >
            <RefreshCw className="size-3.5" />
            体检
          </Button>
        </div>
      </CardHeader>
      <CardContent className="space-y-2 pt-0">
        <Row
          dot={gw ? DOT[gw.health] : "bg-muted-foreground/40"}
          right={
            gw ? (
              <Button
                size="sm"
                variant={running ? "outline" : "default"}
                className="h-7 px-2 text-xs"
                disabled={svcBusy || (!running && !gw.exeExists)}
                onClick={() => void handleServiceAct()}
              >
                {running ? <Power className="size-3.5" /> : <Play className="size-3.5" />}
                {svcBusy ? (running ? "正在停用…" : "正在启用…") : running ? "停用" : "启用"}
              </Button>
            ) : null
          }
        >
          <span className="font-medium">网关本体</span>
          <code className="text-xs text-muted-foreground">:{gw?.port ?? 7863}</code>
          <span className="min-w-0 flex-1 truncate text-xs text-muted-foreground">
            {gw ? describeService(gw) : "还没拿到体检结果"}
          </span>
        </Row>

        <Row
          dot={gw?.health === "ok" ? "bg-emerald-500" : "bg-muted-foreground/40"}
          right={
            <span className="flex items-center gap-2">
              <span className="text-xs text-muted-foreground">常开</span>
              <Switch checked disabled aria-label="OpenAI Chat 常开" title="网关本体 —— 另外两条扩展接口都靠它，关不掉" />
            </span>
          }
        >
          <span className="font-medium">OpenAI Chat</span>
          <code className="text-xs text-muted-foreground">/v1/chat/completions</code>
          <span className="min-w-0 flex-1 truncate text-xs text-muted-foreground">
            pi · dsh · zcode 走这条 —— 网关本体的能力，上游也只给这一种
          </span>
        </Row>

        {status && draft
          ? status.endpoints.map((ep) => {
              const on = draft[ep.id];
              return (
                <Row
                  key={ep.id}
                  dot={dotClass(ep, on, master)}
                  right={
                    <Switch
                      checked={on}
                      disabled={!master || busy}
                      onCheckedChange={(v) => setDraft({ ...draft, [ep.id]: v })}
                      aria-label={`${ep.label} 开关`}
                    />
                  }
                >
                  <span className="font-medium">{ep.label}</span>
                  <code className="text-xs text-muted-foreground">{ep.path}</code>
                  <span className="min-w-0 flex-1 truncate text-xs text-muted-foreground">
                    {ep.client} · {verdict(ep, on, master)}
                  </span>
                </Row>
              );
            })
          : null}

        <div className="flex flex-wrap items-center gap-x-3 gap-y-1 rounded-md border px-2.5 py-2 text-sm">
          <Switch
            checked={master}
            disabled={!status || busy}
            onCheckedChange={(v) => draft && setDraft({ ...draft, enabled: v })}
            aria-label="扩展协议总闸"
          />
          <span className="font-medium">扩展协议总闸</span>
          <span className="min-w-0 flex-1 truncate text-xs text-muted-foreground">
            关掉之后只剩上面的原生 Chat —— Claude Code 和 codex 会连不上
          </span>
        </div>

        <div className="flex flex-wrap items-center gap-2 pt-1">
          <Button
            size="sm"
            variant={dirty ? "default" : "secondary"}
            className="h-7 px-2 text-xs"
            disabled={!dirty || busy || !status?.configExists}
            onClick={() => void handleSaveGates()}
            title="改完开关要重启网关才生效"
          >
            {busy ? "正在重启网关…" : "保存并重启网关"}
          </Button>
          {dirty ? (
            <span className="text-xs text-muted-foreground">
              改动要重启网关才生效（重启时会有几秒钟连不上）
            </span>
          ) : null}
        </div>

        {status?.envOverride ? (
          <p className="text-xs text-amber-600 dark:text-amber-400">
            环境变量 WB2A_PROTOCOL_ENDPOINTS 正设成 off，它会把上面这些开关整个盖掉 —— 改完也不会生效，先去掉它。
          </p>
        ) : null}

        {status && !status.configExists ? (
          <p className="text-xs text-muted-foreground">
            没找到 2api 的配置文件（{status.configPath}）—— 先把下面的程序目录指对。
          </p>
        ) : null}

        <div className="flex flex-wrap items-center gap-2 pt-1">
          <span className="text-xs text-muted-foreground">程序目录</span>
          <Input
            className="h-7 max-w-md flex-1 text-xs"
            value={rootDraft ?? services?.root ?? ""}
            placeholder="2api 的安装路径，比如 D:\w-dev\wb\workbuddy2api"
            onChange={(e) => setRootDraft(e.target.value)}
          />
          {rootDraft != null && rootDraft !== (services?.root ?? "") ? (
            <Button size="sm" variant="outline" className="h-7 px-2 text-xs" onClick={() => void handleSaveRoot()}>
              记下
            </Button>
          ) : null}
        </div>

        {note ? <p className="text-xs text-muted-foreground">{note}</p> : null}
        {err ? <p className="text-xs text-destructive">{err}</p> : null}
        {svcNote ? <p className="text-xs text-muted-foreground">{svcNote}</p> : null}
        {gw?.health === "down" && !svcNote ? (
          <p className="text-xs text-muted-foreground">
            网关不通，Claude Code / codex / pi 就都连不上了 —— 三条接口都在它这一个端口上。
          </p>
        ) : null}
      </CardContent>
    </Card>
  );
}
