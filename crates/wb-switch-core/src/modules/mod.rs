pub mod account;
pub mod activity;
pub mod align;
pub mod auth_file;
pub mod automations;
pub mod checkin;
// （已移除）channel_client —— Centrifugo 通道客户端，零调用者，2026-09-17 归档到
// `.memory/archive/channel_client.rs`（APK 逆向的唯一协议留档，不再参与编译）。
// 云端会话枚举走 HTTP 正门，恢复使用前先看该文件头的红线说明。
pub mod cloud_conv;
pub mod cloud_reconcile;
pub mod codebuddy_cli;
pub mod codebuddy_cn_ide;
pub mod codebuddy_ide;
pub mod codebuddy_ide_session;
pub mod codebuddy_ide_session_sync;
pub mod config;
pub mod credit_ledger;
pub mod credit_usage;
pub mod credits;
pub mod discover;
pub mod error_log;
pub mod export_import;
/// 成长任务自动执行（夜猫子 + 开学季）——2api 移植，发布 fork 前随私有功能整体剔除。
pub mod growth_tasks;
// gateway(私有) —— 网关凭证跟随同步（v3.2 跟随模式）；摘取上游 PR 时整体剔除。
pub mod gateway_protocol;
pub mod gateway_services;
pub mod gateway_sync;
pub mod jetbrains;
pub mod limits;
#[cfg(target_os = "linux")]
pub mod linux_keyring;
pub mod notifications;
pub mod oauth;
pub mod official_usage;
pub mod oplog;
pub mod process;
pub mod rate_limit_events;
pub mod rate_limit_hook;
// 原 projects_anchor（项目锚点同步已于 2026-09-17 弃用，只留会话瘦身）⇒ 改名如实反映职责。
pub mod session_slim;
pub mod renderer_refresh;
pub mod refresh;
pub mod rotate;
pub mod session;
pub mod session_share;
pub mod session_backup;
pub mod session_groups;
pub mod session_link;
pub mod switch;
/// 切换流程的本地私有编排（oplog 留痕 / dry_run 预览 / autoLink / 对齐 / sid 改写）。
/// switch.rs 只留挂钩点，本地逻辑住这里——上游演进切换主体时零冲突。
pub mod switch_flow;
pub mod token_stats;
pub mod travel;
pub mod ui_theme;
pub mod update;
pub mod variant;
pub mod vscode_cn_inject;
pub mod vscode_ext;
pub mod vscode_session;
pub mod vscode_session_link;
pub mod vscode_session_sync;
