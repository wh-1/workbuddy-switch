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

/** 单账号池条目：对齐 2api internal/pool.Status（宽松取用，缺字段不崩）。
 *  字段**以 `GET /status` 实测返回为准**（2026-09-22 核对）—— 后端不返回的字段不在此声明，
 *  否则 UI 误用后恒显示 `-`（实测踩过：`err_total` 恒 undefined 让「成功率」永远 100%）。 */
export interface GatewayAccountStatus {
  uid: string;
  realm?: string;
  nickname?: string;
  credits?: number;
  cooling?: boolean;
  /** 冷却结束时刻（软冷却；剩余时间用 `remainingSecFrom(until)` 现算，后端不再单独给秒数）。 */
  until?: string;
  disabled?: boolean;
  manual_disabled?: boolean;
  success_count?: number;
  last_success?: string;
  last_err?: string;
  /** 连续失败计数（触发降级/熔断的观测量）。 */
  consecutive_fails?: number;
  /** 降级结束时刻：仍在未来即在降级冷却中。 */
  degrade_until?: string;
  in_flight?: number;
  breaker_fails?: number;
  breaker_until?: string;
  rate_limited_models?: GatewayRateLimitedModel[];
  /** 按 (账号, 模型) 的单价观测台账（0 = 实测免费）。 */
  model_costs?: GatewayCostModel[];
}

export interface GatewayRateLimitedModel {
  model?: string;
  /** 网关自家软冷却结束时刻（429 起 600s 指数退避、封顶 2h）。 */
  until?: string;
  /** 上游权威重置时刻 —— 实测可与 `until` 差数小时（18:25 vs 22:08），**展示与判断应以它为准**。 */
  reset_at?: string;
  reason?: string;
  [k: string]: unknown;
}

/** 成本台账的一行：某个模型在该账号上的观测单价。 */
export interface GatewayCostModel {
  model?: string;
  /** 每千 token 折算单价（0 = 实测免费）。 */
  cost_per_1k?: number;
  last_seen?: string;
  samples?: number;
}

/** `/status` 顶层按域的汇总（cn / global）。 */
export interface GatewayRealmTotals {
  cooling?: number;
  disabled?: number;
  healthy?: number;
  in_flight_full?: number;
  total?: number;
}

/** `/status` 顶层：池级权威计数。
 *  ⚠️ **优先消费这些字段，别在前端从 accounts 自算** —— 自算版与后端口径不同（算不出 `in_flight_full`，
 *  也不含 realm 维度），会形成同一事实两处实现。 */
export interface GatewayStatus {
  accounts?: GatewayAccountStatus[];
  /** 可服务账号数（后端权威口径，已计入全部状态位）。 */
  healthy?: number;
  total?: number;
  cooling?: number;
  disabled?: number;
  /** 在途已占满（不再参与选号）的账号数。 */
  in_flight_full?: number;
  realm_totals?: Record<string, GatewayRealmTotals>;
  /** 当前粘性会话绑定数。 */
  sticky_sessions?: number;
  /** 状态镜像模式：`noop` = 仅本地落盘。 */
  redis_mode?: string;
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

/** 由 ISO 截止时刻现算剩余秒数（`until` 类字段用；已过期或非法返回 0）。 */
export function remainingSecFrom(iso?: string): number {
  if (!iso) return 0;
  const t = new Date(iso).getTime();
  if (Number.isNaN(t)) return 0;
  return Math.max(0, Math.round((t - Date.now()) / 1000));
}

/** 该 ISO 时刻是否仍在未来（`breaker_until` / `degrade_until` 判活跃用）。 */
export function isFuture(iso?: string): boolean {
  if (!iso) return false;
  const t = new Date(iso).getTime();
  return !Number.isNaN(t) && t > Date.now();
}

export function formatTime(iso?: string): string {
  if (!iso) return "-";
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return "-";
  return d.toLocaleString("zh-CN", { month: "2-digit", day: "2-digit", hour: "2-digit", minute: "2-digit" });
}
