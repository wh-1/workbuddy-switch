// gateway(私有) —— 本文件与网关页均为私有组件，摘取上游 PR 时整体剔除。
// 2api（workbuddy2api）网关的轻量 HTTP 客户端：只读监控用。

export interface GatewayConfig {
  baseUrl: string;
  apiKey: string;
  enabled: boolean;
}

const LS_KEY = "gateway.config.v1";
const DEFAULT_BASE = "http://127.0.0.1:7863";

export function loadGatewayConfig(): GatewayConfig {
  try {
    const raw = localStorage.getItem(LS_KEY);
    if (raw) {
      const parsed = JSON.parse(raw) as Partial<GatewayConfig>;
      return {
        baseUrl: parsed.baseUrl?.trim() || DEFAULT_BASE,
        apiKey: parsed.apiKey || "",
        enabled: parsed.enabled === true,
      };
    }
  } catch {
    // 损坏的本地配置按默认处理
  }
  return { baseUrl: DEFAULT_BASE, apiKey: "", enabled: false };
}

export function saveGatewayConfig(cfg: GatewayConfig): void {
  localStorage.setItem(LS_KEY, JSON.stringify(cfg));
}

/** 单账号池条目：对齐 2api internal/pool.Status（宽松取用，缺字段不崩）。 */
export interface GatewayAccountStatus {
  uid: string;
  realm?: string;
  nickname?: string;
  credits?: number;
  cooling?: boolean;
  cool_kind?: string;
  cool_remaining_sec?: number;
  until?: string;
  reason?: string;
  soft_streak?: number;
  disabled?: boolean;
  disabled_reason?: string;
  manual_disabled?: boolean;
  manual_reason?: string;
  success_count?: number;
  err_total?: number;
  last_success?: string;
  last_err?: string;
  in_flight?: number;
  breaker_fails?: number;
  breaker_until?: string;
  rate_limited_models?: GatewayRateLimitedModel[];
}

export interface GatewayRateLimitedModel {
  model?: string;
  until?: string;
  [k: string]: unknown;
}

export interface GatewayStatus {
  accounts?: GatewayAccountStatus[];
  [k: string]: unknown;
}

export interface GatewayHealthz {
  service?: string;
  version?: string;
  [k: string]: unknown;
}

export type GatewayState = "idle" | "connecting" | "online" | "offline";

async function gatewayFetch<T>(path: string, cfg: GatewayConfig, timeoutMs = 8000): Promise<T> {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), timeoutMs);
  try {
    const res = await fetch(cfg.baseUrl.replace(/\/+$/, "") + path, {
      headers: cfg.apiKey ? { Authorization: `Bearer ${cfg.apiKey}` } : undefined,
      signal: controller.signal,
    });
    if (!res.ok) {
      throw new Error(`HTTP ${res.status}${res.status === 401 ? "（API Key 不对）" : ""}`);
    }
    return (await res.json()) as T;
  } catch (e) {
    if (e instanceof DOMException && e.name === "AbortError") {
      throw new Error("连接超时（网关没响应）");
    }
    throw e instanceof Error ? e : new Error(String(e));
  } finally {
    clearTimeout(timer);
  }
}

/** /healthz 免鉴权，用于探活与服务身份识别。 */
export function fetchGatewayHealthz(cfg: GatewayConfig): Promise<GatewayHealthz> {
  return gatewayFetch<GatewayHealthz>("/healthz", cfg, 4000);
}

/** /status 需要鉴权；返回账号池全景。 */
export function fetchGatewayStatus(cfg: GatewayConfig): Promise<GatewayStatus> {
  return gatewayFetch<GatewayStatus>("/status", cfg);
}

export type GatewayAdminAction = "disable" | "enable" | "revive";

/** /admin/* 的统一响应体（2api internal/server/admin.go adminState）。 */
export interface GatewayAdminState {
  uid: string;
  manual_disabled: boolean;
  manual_reason?: string;
  disabled: boolean;
  changed: boolean;
  [k: string]: unknown;
}

/**
 * 运维动作（2api `admin.enabled=true` 时可用；鉴权与 /status 同源）：
 * - disable：手动停用（独立 manual_disabled 位，对话流量摘除，签到/保活照常）
 * - enable：解除手动停用（若账号仍被系统自动禁用，还需 revive）
 * - revive：解除系统自动禁用
 * 全部幂等；重复调用只更新原因文案。
 */
export async function gatewayAdminAction(
  cfg: GatewayConfig,
  uid: string,
  action: GatewayAdminAction,
  reason?: string,
): Promise<GatewayAdminState> {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), 8000);
  try {
    const res = await fetch(
      `${cfg.baseUrl.replace(/\/+$/, "")}/admin/accounts/${encodeURIComponent(uid)}/${action}`,
      {
        method: "POST",
        headers: {
          ...(cfg.apiKey ? { Authorization: `Bearer ${cfg.apiKey}` } : {}),
          ...(reason ? { "Content-Type": "application/json" } : {}),
        },
        body: reason ? JSON.stringify({ reason }) : undefined,
        signal: controller.signal,
      },
    );
    if (!res.ok) {
      const text = await res.text().catch(() => "");
      let detail = "";
      try {
        detail = (JSON.parse(text)?.error?.message as string) || "";
      } catch {
        // 非 JSON 错误体，忽略
      }
      const hint =
        res.status === 404
          ? "（账号不在池里，或 2api 未开 admin.enabled）"
          : res.status === 401
            ? "（API Key 不对）"
            : "";
      throw new Error(`HTTP ${res.status}${detail ? `：${detail}` : hint}`);
    }
    return (await res.json()) as GatewayAdminState;
  } catch (e) {
    if (e instanceof DOMException && e.name === "AbortError") {
      throw new Error("操作超时（网关没响应）");
    }
    throw e instanceof Error ? e : new Error(String(e));
  } finally {
    clearTimeout(timer);
  }
}

export function formatCountdown(sec?: number): string {
  if (sec == null || sec <= 0) return "-";
  if (sec < 60) return `${sec}s`;
  if (sec < 3600) return `${Math.floor(sec / 60)}m${sec % 60}s`;
  return `${Math.floor(sec / 3600)}h${Math.floor((sec % 3600) / 60)}m`;
}

export function formatTime(iso?: string): string {
  if (!iso) return "-";
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return "-";
  return d.toLocaleString("zh-CN", { month: "2-digit", day: "2-digit", hour: "2-digit", minute: "2-digit" });
}
