// gateway(私有) —— 网关跟随同步（v3.2）的 Tauri 命令封装。
// 摘取上游 PR 时与 gateway_sync.rs、GatewayPage/AccountsPage 相关改动整体剔除。
import { invoke } from "@tauri-apps/api/core";

export interface GatewaySyncConfig {
  enabled: boolean;
  authsDir: string;
}

export interface GatewaySyncStatus {
  enabled: boolean;
  authsDir: string;
  /** switch 当前登录账号 uid；取不到为 null。 */
  localUid: string | null;
  /** 当前登录账号展示名（email → nickname → uid，与账号管理页同源）。 */
  localName: string | null;
  /** 网关 auths/workbuddy-current.json 里的 uid；未同步/未配置为 null。 */
  gatewayUid: string | null;
  /** 网关池当前号的账号名。 */
  gatewayName: string | null;
  /** 两边 uid 是否一致（false = 标红 + 一键重同步）。 */
  uidMatch: boolean;
  /** 上次同步失败的 uid（重试队列，非空说明有待补偿）。 */
  pendingUid: string | null;
  lastSyncAt: number | null;
  lastSyncReason: string | null;
  currentFile: string | null;
}

export interface GatewaySyncResult {
  ok: boolean;
  action: "synced" | "skipped" | "failed" | "disabled";
  uid?: string;
  reason?: string;
  error?: string;
  path?: string;
}

export function fetchGatewaySyncStatus(): Promise<GatewaySyncStatus> {
  return invoke<GatewaySyncStatus>("get_gateway_sync_status");
}

export function fetchGatewaySyncConfig(): Promise<GatewaySyncConfig> {
  return invoke<GatewaySyncConfig>("get_gateway_config");
}

export function saveGatewaySyncConfig(cfg: GatewaySyncConfig): Promise<GatewaySyncConfig> {
  return invoke<GatewaySyncConfig>("save_gateway_config", { config: cfg });
}

export function resyncGateway(): Promise<GatewaySyncResult> {
  return invoke<GatewaySyncResult>("gateway_resync");
}
