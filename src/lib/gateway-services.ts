// gateway(私有) —— 本地网关服务的启停与体检命令封装。
// 服务清单真源在 core 的 modules/gateway_services.rs；2026-09-23 协议端点内嵌后
// 清单只剩网关一项。摘取上游 PR 时整体剔除。
import { invoke } from "@tauri-apps/api/core";

/** 单服务体检结果。health 三态：
 *  - ok       ：端口通 + /healthz 2xx + 身份字段对得上
 *  - degraded ：端口通但对不上（服务不对 / HTTP 异常）——**不算可用**，UI 点黄
 *  - down     ：端口没开
 */
export interface GatewayServiceHealth {
  id: string;
  label: string;
  port: number;
  desc: string;
  health: "ok" | "degraded" | "down";
  portOpen: boolean;
  httpOk: boolean;
  kindMatch: boolean;
  kind: string | null;
  latencyMs: number;
  pid: number | null;
  exe: string | null;
  exeExists: boolean;
  message: string | null;
}

export interface GatewayServicesStatus {
  root: string;
  services: GatewayServiceHealth[];
  summary: { ok: number; degraded: number; down: number };
  checkedAt: number;
}

export interface GatewayServicesConfig {
  root: string;
  services: Record<string, { exe?: string; task?: string; disabled?: boolean }>;
}

export interface GatewayServiceActionResult {
  id: string;
  action: string;
  port?: number;
  via?: string;
  pid?: number;
  actions?: string[];
  message?: string;
  error?: string;
  /** start_all 才有：起完全量后的最新体检快照。 */
  status?: GatewayServicesStatus;
  results?: GatewayServiceActionResult[];
}

export function fetchGatewayServicesStatus(): Promise<GatewayServicesStatus> {
  return invoke<GatewayServicesStatus>("get_gateway_services_status");
}

export function saveGatewayServicesConfig(
  cfg: GatewayServicesConfig,
): Promise<GatewayServicesConfig> {
  return invoke<GatewayServicesConfig>("save_gateway_services_config", { config: cfg });
}

export function startGatewayService(id: string): Promise<GatewayServiceActionResult> {
  return invoke<GatewayServiceActionResult>("start_gateway_service", { id });
}

export function stopGatewayService(id: string): Promise<GatewayServiceActionResult> {
  return invoke<GatewayServiceActionResult>("stop_gateway_service", { id });
}
