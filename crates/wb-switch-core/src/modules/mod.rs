pub mod account;
pub mod align;
pub mod auth_file;
pub mod checkin;
// （已移除）channel_client —— Centrifugo 通道客户端，零调用者，2026-09-17 归档到
// `.memory/archive/channel_client.rs`（APK 逆向的唯一协议留档，不再参与编译）。
// 云端会话枚举走 HTTP 正门，恢复使用前先看该文件头的红线说明。
pub mod cloud_conv;
pub mod cloud_reconcile;
pub mod codebuddy_cli;
pub mod codebuddy_cn_ide;
pub mod codebuddy_ide;
pub mod config;
pub mod credit_ledger;
pub mod credit_usage;
pub mod credits;
pub mod discover;
pub mod export_import;
pub mod limits;
// ⚠️ 原名 limits.rs，与上游 feat/model-rate-limit-ledger 的同名模块撞车 ⇒ 改名避让。
// 上游 v0.1.40 起两份并存作 A/B 对照：**UI 只接上游**，本模块仅后端留档（接线见 commands_local.rs）。
pub mod limits_local;
pub mod oauth;
pub mod official_usage;
pub mod oplog;
pub mod process;
pub mod rate_limit_events;
pub mod rate_limit_hook;
// 原 projects_anchor（项目锚点同步已于 2026-09-17 弃用，只留会话瘦身）⇒ 改名如实反映职责。
pub mod session_slim;
pub mod refresh;
pub mod rotate;
pub mod session;
pub mod session_share;
pub mod switch;
pub mod token_stats;
pub mod travel;
pub mod ui_theme;
pub mod update;
pub mod variant;
pub mod vscode_cn_inject;
