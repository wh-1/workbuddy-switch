// gateway(私有) —— 协议端点（Anthropic Messages / OpenAI Responses）的状态与开关封装。
// 摘取上游 PR 时整体剔除。
//
// 真源：core 的 modules/gateway_protocol.rs（探活判据 = 响应头 X-Service；
// 写开关 = 最小编辑 2api 的 config.json）。改完开关**必须重启网关**才生效。
import { invoke } from "@tauri-apps/api/core";

export interface GatewayProtocolGates {
  /** 扩展协议总闸：关掉等于不挂任何下游协议端点（**不是网关总开关** —— 网关启停在 `gateway-services`）。
   *  原生 OpenAI Chat 由网关本体提供、不受它影响，所以别把这一项叫成「总开关」。 */
  enabled: boolean;
  anthropic: boolean;
  responses: boolean;
  /** 有效值 = 总闸 && 分闸 —— 展示"能不能用"要看这两个。 */
  anthropicEffective: boolean;
  responsesEffective: boolean;
}

export interface GatewayProtocolEndpoint {
  id: "anthropic" | "responses";
  label: string;
  path: string;
  /** 走这条协议的客户端（给使用者看的对应关系）。 */
  client: string;
  /** 探活结论：响应头 X-Service 对得上才算挂上。 */
  mounted: boolean;
  status: number | null;
  service: string | null;
  detail: string | null;
}

export interface GatewayProtocolStatus {
  root: string;
  configPath: string;
  /** 找不到配置文件时为 false —— 此时开关不可写。 */
  configExists: boolean;
  port: number;
  gates: GatewayProtocolGates;
  /** 配置里是否显式写了 protocol_endpoints 节（不写 = 2api 按全开处理）。 */
  configured: boolean;
  /** 环境变量 WB2A_PROTOCOL_ENDPOINTS=off 会整体覆盖文件开关（排障用）。 */
  envOverride: boolean;
  endpoints: GatewayProtocolEndpoint[];
  checkedAt: number;
}

export interface GatewayProtocolSaveResult {
  changed: boolean;
  /** true 表示文件真改了 ⇒ 需要重启网关才生效。 */
  restartRequired: boolean;
  gates: GatewayProtocolGates;
  /** 改动前的原件备份路径（改了才有）。 */
  backup: string | null;
  message: string;
}

export function fetchGatewayProtocolStatus(): Promise<GatewayProtocolStatus> {
  return invoke<GatewayProtocolStatus>("get_gateway_protocol_status");
}

export function saveGatewayProtocolGates(
  gates: Pick<GatewayProtocolGates, "enabled" | "anthropic" | "responses">,
): Promise<GatewayProtocolSaveResult> {
  return invoke<GatewayProtocolSaveResult>("save_gateway_protocol_gates", { gates });
}

export function restartGatewayService(
  id: string,
): Promise<{ action?: string; message?: string; error?: string }> {
  return invoke("restart_gateway_service", { id });
}
