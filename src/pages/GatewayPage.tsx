// gateway(私有) —— 本页面为私有组件，摘取上游 PR 时整体剔除。
// 2api（workbuddy2api）网关的轻量运维面板（v3.2 跟随模式）：
// 池里永远只有 switch 当前账号（由跟随同步自动维护），本页只读监控。

import { useCallback, useEffect, useState } from "react";
import { Globe } from "lucide-react";

import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Skeleton } from "@/components/ui/skeleton";
import { GatewayConnectionBar } from "@/components/gateway/GatewayConnectionBar";
import { GatewayOverviewCards } from "@/components/gateway/GatewayOverviewCards";
import { GatewayInterfaceCard } from "@/components/gateway/GatewayInterfaceCard";
import { AccountPoolTable } from "@/components/gateway/AccountPoolTable";
import { GatewaySyncCard } from "@/components/gateway/GatewaySyncCard";
import { RateLimitLedger } from "@/components/gateway/RateLimitLedger";
import { ModelCostLedger } from "@/components/gateway/ModelCostLedger";
import {
  fetchGatewayHealthz,
  fetchGatewayStatus,
  loadGatewayConfig,
  saveGatewayConfig,
  type GatewayConfig,
  type GatewayHealthz,
  type GatewayState,
  type GatewayStatus,
} from "@/lib/gateway";
import { fetchGatewaySyncStatus } from "@/lib/gateway-sync";
import {
  fetchGatewayServicesStatus,
  type GatewayServicesStatus,
} from "@/lib/gateway-services";

export default function GatewayPage() {
  const [config, setConfig] = useState<GatewayConfig>(() => loadGatewayConfig());
  const [state, setState] = useState<GatewayState>("idle");
  const [error, setError] = useState<string | null>(null);
  const [healthz, setHealthz] = useState<GatewayHealthz | null>(null);
  const [status, setStatus] = useState<GatewayStatus | null>(null);
  const [localUid, setLocalUid] = useState<string | null>(null);
  const [localName, setLocalName] = useState<string | null>(null);
  /** 网关进程体检：在页面统一拉，传给「网关接口」卡（同一份数据只轮询一处）。 */
  const [services, setServices] = useState<GatewayServicesStatus | null>(null);

  const pullServices = useCallback(() => {
    void fetchGatewayServicesStatus()
      .then(setServices)
      .catch(() => {});
  }, []);

  const refresh = useCallback(async (cfg: GatewayConfig, silent = false) => {
    if (!silent) {
      setState("connecting");
      setError(null);
    }
    try {
      const hz = await fetchGatewayHealthz(cfg);
      setHealthz(hz);
      const st = await fetchGatewayStatus(cfg);
      setStatus(st);
      setState("online");
      if (silent) setError(null);
    } catch (e) {
      if (silent) return; // 静默轮询失败不打断在线态，等下一轮再试
      setHealthz(null);
      setStatus(null);
      setState("offline");
      setError(e instanceof Error ? e.message : String(e));
    }
  }, []);

  useEffect(() => {
    if (config.enabled || config.apiKey || config.baseUrl) {
      void refresh(config);
    }
    // 仅首次挂载自动连一次；之后由用户手动刷新/保存触发
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // 跟随同步状态（uid 一致性比对用；30s 轮询与面板节奏一致）
  useEffect(() => {
    const pull = () => {
      void fetchGatewaySyncStatus()
        .then((s) => {
          setLocalUid(s.localUid);
          setLocalName(s.localName);
        })
        .catch(() => {});
    };
    pull();
    const timer = setInterval(() => {
      if (!document.hidden) pull();
    }, 30_000);
    return () => clearInterval(timer);
  }, []);

  // 网关进程每 15s 体检一次，页面切到后台就跳过。
  useEffect(() => {
    pullServices();
    const timer = setInterval(() => {
      if (!document.hidden) pullServices();
    }, 15_000);
    return () => clearInterval(timer);
  }, [pullServices]);

  // 在线时每 30s 静默轮询一次，让面板自己保持新鲜；页面切到后台就跳过。
  useEffect(() => {
    if (state !== "online") return;
    const timer = setInterval(() => {
      if (document.hidden) return;
      void refresh(config, true);
    }, 30_000);
    return () => clearInterval(timer);
  }, [state, config, refresh]);

  const handleSave = (cfg: GatewayConfig) => {
    setConfig(cfg);
    saveGatewayConfig({ ...cfg, enabled: true });
    void refresh(cfg);
  };

  const accounts = status?.accounts ?? [];
  /** 离线分两种麻烦：进程没跑 vs 连不上 —— 提示必须分开，否则用户只能瞎猜。 */
  const gwService = services?.services.find((s) => s.id === "2api") ?? null;

  return (
    <div className="space-y-4 p-4">
      <div className="flex items-center gap-2">
        <Globe className="size-5 text-muted-foreground" />
        <h1 className="text-lg font-semibold">网关</h1>
        <span className="text-xs text-muted-foreground">2api · workbuddy2api（第三方组件，MIT © Sliverkiss · 非官方，仅限本人授权账号与私有环境）</span>
      </div>

      <GatewayConnectionBar
        config={config}
        state={state}
        error={error}
        serviceName={typeof healthz?.service === "string" ? healthz.service : undefined}
        onSave={handleSave}
        onRefresh={() => void refresh(config)}
      />

      <GatewayInterfaceCard services={services} onRefreshServices={pullServices} />

      <GatewaySyncCard />

      {state === "connecting" ? (
        <div className="space-y-3">
          <div className="grid grid-cols-2 gap-3 lg:grid-cols-4">
            {[0, 1, 2, 3].map((i) => (
              <Skeleton key={i} className="h-20 rounded-lg" />
            ))}
          </div>
          <Skeleton className="h-40 rounded-lg" />
        </div>
      ) : state === "online" && status?.accounts ? (
        <div className="space-y-4">
          <GatewayOverviewCards status={status ?? {}} accounts={accounts} />
          {accounts.length === 0 ? (
            <Card>
              <CardContent className="py-8 text-center text-sm text-muted-foreground">
                网关在线，账号池还空着。在「设置」里开启跟随同步并填好网关 auths 目录，
                之后每次切号都会自动把当前账号同步过去，不用手动加。
              </CardContent>
            </Card>
          ) : (
            <>
              {/* 运维动作（摘除/恢复/复活）随池概念一起退役（v3.1 步骤 4，代码保留 UI 移除）：
                  跟随模式下池里只有当前号，手动摘掉等于自己把网关停了。 */}
              <AccountPoolTable accounts={accounts} localUid={localUid} localName={localName} />
              <RateLimitLedger accounts={accounts} />
              <ModelCostLedger accounts={accounts} />
            </>
          )}
        </div>
      ) : state === "offline" ? (
        <Card>
          <CardHeader className="pb-2">
            <CardTitle className="text-sm">网关不在线</CardTitle>
          </CardHeader>
          <CardContent className="space-y-1.5 text-sm text-muted-foreground">
            {gwService == null ? (
              <p>
                2api 网关要自己先跑起来（本机用计划任务 <code className="font-mono">wb2api-gateway</code> 常驻，
                默认 <code className="font-mono">127.0.0.1:7863</code>）。跑起来后点上面的「刷新」。
              </p>
            ) : gwService.health === "down" ? (
              <p>
                网关进程没在跑（计划任务 <code className="font-mono">wb2api-gateway</code>）。
                先在上面「网关接口」卡的第一行点「启用」，起来以后回这里点「刷新」。
              </p>
            ) : (
              <p>
                网关进程在跑，但这边连不上 —— 多半是地址或 API Key 对不上（默认
                <code className="font-mono">127.0.0.1:7863</code>）。核对上面两项后点「保存并连接」。
              </p>
            )}
            <p>
              跟随模式下网关只用当前账号，切号后会自动同步过去，不需要两边各自维护登录态。
            </p>
          </CardContent>
        </Card>
      ) : (
        <Card>
          <CardHeader className="pb-2">
            <CardTitle className="text-sm">先连上网关</CardTitle>
          </CardHeader>
          <CardContent className="text-sm text-muted-foreground">
            填上网关地址和 API Key，点「保存并连接」，就能在这里看到账号池、冷却和限流情况了。
          </CardContent>
        </Card>
      )}
    </div>
  );
}
