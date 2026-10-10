# workbuddy-switch

WorkBuddy、CodeBuddy IDE、CodeBuddy CLI 与 VS Code CodeBuddy 插件账号切换桌面 App（Tauri），四者均支持国内版 / 国际版，并提供积分到期与 Token 用量监控。

<p align="center">
  <img src="public/icon-transparent.png" alt="WorkBuddy Switch 图标" width="128" />
</p>

多账号共享登录态，一键切换 WorkBuddy 登录账号。**会话复制**：把当前账号的会话以新 id 复制给目标账号，源账号数据不受影响，云端归属目标账号。**关联会话**：复制过的会话会自动建立跨账号关联，集中查看各账号副本状态，并把新增内容同步过去。

## ★ 核心需求：无感切号 + 继续会话（开发维护的前提，不是可选项）

- **三层缺一不可**：切号后设置/记忆/人格/文件/会话**全跟随**，用户感觉不到换号 —— ① 跨档位会话关联（上游 `session_link`）② **跨账号会话共享**（私有 autoLink）③ **跨账号记忆·设置·文件同步**（私有 `align`，上游**没有**）
- **回归判定**：切号后「会话在、记忆不在」「设置回默认」「AI 像换了个人」= 回归。⛔ 实证反例：只做 ② 不做 ③ ⇒ 切号后 `user-<uid>-personal/MEMORY.md` 是空模板，用户级记忆缺席。校验 `scripts/analysis/account_consistency_probe.py`

**在线演示**：[打开 GitHub Pages 演示](https://changexbc.github.io/workbuddy-switch/)（只读演示；账号、积分与请求记录均为虚构数据，所有业务操作均已禁用，另含只读的会话悬浮栏演示）

## 快速开始

### npm 安装（webui）

```bash
npm i -g workbuddy-switch
workbuddy-switch              # 启动本地服务 + 自动打开浏览器
workbuddy-switch status       # 终端查看当前账号
```

webui 界面与桌面 App 一致：WorkBuddy / CodeBuddy CLI / CodeBuddy IDE / VS Code CodeBuddy 插件账号切换、积分到期监控、自动签到、会话复制、Token 统计与 token 保活。

服务默认开在 `57890`。这个端口万一用不了（Windows 上挺常见 —— Hyper-V / WSL / Docker 会
预留一段端口，落在里面的端口谁都绑不上），它会自动往后找一个能用的，终端里那行
`webui: http://...` 就是实际地址。想固定端口：`workbuddy-switch --port 58090`。

### 桌面 App

前往 [GitHub Releases](https://github.com/changexbc/workbuddy-switch/releases/latest) 下载对应平台的安装包：

| 平台 | 安装包 | 安装方式 |
| --- | --- | --- |
| macOS Apple Silicon（M 系列，arm64） | `workbuddy-switch_<版本>_aarch64.dmg` | 打开 DMG，将 `workbuddy-switch.app` 拖入「应用程序」 |
| macOS Intel（x86_64） | `workbuddy-switch_<版本>_x86_64.dmg` | 打开 DMG，将 `workbuddy-switch.app` 拖入「应用程序」 |
| Windows x64 | `workbuddy-switch_<版本>_x64-setup.exe` | 运行安装程序并按提示完成安装 |
| Linux x64 | `workbuddy-switch_<版本>_amd64.deb` / `workbuddy-switch_<版本>_amd64.AppImage` | Debian/Ubuntu 安装 `.deb`；其他发行版可给 AppImage 添加执行权限后直接运行 |

macOS 首次启动若提示无法验证开发者，先在 Finder 中按住 Control 点击应用并选择「打开」，或前往「系统设置 → 隐私与安全性」选择「仍要打开」。仅当安装包来自上述官方 Releases、且系统仍提示「已损坏」时，再执行：

```bash
xattr -rd com.apple.quarantine "/Applications/workbuddy-switch.app"
```

应用能启动但切换账号时提示无权限，请参阅下方 [macOS 权限说明](#macos-权限说明)。

另有 npm / webui 版本可在浏览器中使用，见文末 [npm / webui 版本](#npm--webui-版本)。

## 功能

| 模块 | 说明 |
| --- | --- |
| 账号管理 | OAuth 扫码登录、导入导出账号、删除账号 |
| 账号切换 | 一键切换 WorkBuddy 登录账号，切换过程实时显示进度 |
| 会话复制 | 把当前账号勾选的会话复制给目标账号，源账号数据不受影响 |
| 关联会话 | 复制过的会话自动建立跨账号关联；按客户端集中查看同一会话在各账号中的副本状态，支持增量同步、分歧处理与新增 / 解除关联 |
| 积分到期查询 | 自动查询每个账号的积分剩余量与到期时间；7 天内到期高亮，并按紧迫程度排序、标注「建议优先使用」 |
| 积分统计 | 汇总官方请求用量：总览、近 30 天趋势、模型分类、账号消耗与请求明细 |
| Token 统计 | 按来源查看 Token 总览与趋势，含构成占比、活跃热力图、项目/模型 Top 10 与会话排行 |
| CodeBuddy CLI | 与 WorkBuddy 复用同一账号库，默认账号独立；切换后立即生效，无需重启 CLI |
| CodeBuddy IDE | 支持切换 CodeBuddy IDE 桌面客户端账号，并可在弹窗中勾选复制会话，与 CodeBuddy CLI 相互独立 |
| VS Code CodeBuddy 插件 | 支持切换 VS Code 内的 CodeBuddy 插件账号；VS Code 运行时可自动关闭并在写入后重新打开 |
| JetBrains IDE 插件 | 支持切换 IntelliJ IDEA / PyCharm 内的 CodeBuddy 插件账号，一次切换写入所有装了插件的 IDE；IDE 运行时可自动关闭并在写入后重新打开 |
| 插件会话复制 | 切换插件账号时，可把当前插件账号的会话复制给目标账号（加法，源账号不变） |
| 自动轮换 | 后台把积分最紧迫的账号设为 CodeBuddy CLI 后续启动账号；检测到 CLI 会话运行时会跳过 |
| 自动更新 | 从 GitHub Releases 检查新版本，整包更新经签名校验 |
| 会话悬浮窗 | 桌面版内置 Agent Companion 悬浮栏，在桌面集中显示 Codex / WorkBuddy / CodeBuddy / Codeg 会话的运行中 / 待确认 / 已完成状态；悬停查看详情，支持跳转时点击回到原会话，托盘可临时隐藏 |
| 权限检测 | macOS 授权引导（App 管理 / 完全磁盘访问拖拽授权 + 自动检测） |

## 支持的工具

| 工具 | 账号切换 | 会话复制 | 自动关闭重开 | 自动轮换 | 悬浮窗监听 |
| --- | :---: | :---: | :---: | :---: | :---: |
| WorkBuddy | ✅ | ✅ | ✅ | — | ✅ |
| CodeBuddy IDE | ✅ | ✅ | ✅ | — | ✅ |
| CodeBuddy CLI | ✅ | — | — | ✅ | — |
| VS Code CodeBuddy 插件 | ✅ | ✅ | ✅ | — | ✅ |
| JetBrains IDE 插件（IDEA / PyCharm） | ✅ | — | ✅ | — | — |

✅ 表示支持，— 表示不支持。设置 →「支持工具」可按客户端逐个开启 / 关闭入口；关闭后该端入口与状态轮询一并隐藏，不影响账号库与其它端；JetBrains 端默认关闭，可在设置中随时打开。

CodeBuddy CLI 切换时会先关闭正在运行的 CLI，当前会话会中断且不会自动重开；其余各端可在客户端运行时自动完成切换。

### 会话悬浮窗（Agent Companion）

桌面版内置 [Agent Companion](https://github.com/changexbc/agent-companion) 悬浮栏：把各 AI Agent 的任务状态集中到桌面，一眼看出谁还在运行、谁需要你确认，支持跳转时点击即可回到原会话；悬浮栏可拖动调整位置，托盘可随时显示 / 隐藏。

| 监听来源 | 跳转到指定会话 | 点击后的行为 |
| --- | :---: | --- |
| Codex（Desktop / CLI） | ✅ | 打开 Codex Desktop 中的指定任务 |
| WorkBuddy（国内版 / 国际版） | ✅ | 打开对应版本中的指定对话 |
| CodeBuddy IDE（国内版 / 国际版） | — | 有工程路径时打开工程，否则只唤起 CodeBuddy |
| CodeBuddy VS Code 插件 | — | 尝试打开会话所属的 VS Code 工程，无法确定时只唤起 VS Code |
| Codeg | ✅ | 打开 Codeg 中的指定聊天会话 |

各来源都会显示运行中 / 待确认 / 已完成状态；CodeBuddy CLI 与 JetBrains 插件不在监听范围内。开启方式：左下角悬浮窗图标，或设置 → Agent Companion；首次使用在「悬浮窗设置」中完成接入（依赖对应客户端的 Hooks / Webhook），监听来源与外观样式也在那里调整。

网页演示里的悬浮栏：一个已完成的 Codex 会话与一个失败的 WorkBuddy 会话，各自弹出信息卡（截自[在线演示](https://changexbc.github.io/agent-companion/)，数据为虚构）。

![Agent Companion 悬浮栏演示：已完成与失败两种状态各自弹出信息卡](docs/images/agent-companion-demo-rail.png)

更多说明与独立版见 [Agent Companion 仓库](https://github.com/changexbc/agent-companion) · [在线演示](https://changexbc.github.io/agent-companion/)

## 使用

1. **添加与导出账号**：账号页 →「OAuth 扫码登录」「导入备份」；「导出」可将勾选账号备份为 JSON
2. **切换账号与账号信息**：账号卡片 →「切换」，可勾选复制当前会话；「账号信息」可给账号添加备注，并选择卡片上显示账号名 / 手机号 / 备注
3. **查看积分与统计**：账号页自动查询各账号积分到期情况，点「刷新积分」手动更新；侧栏进入「积分统计」「Token 统计」查看用量明细
4. **切换各客户端账号**：CodeBuddy CLI、CodeBuddy IDE、VS Code CodeBuddy 插件均可在账号卡片一键切换；CodeBuddy IDE 与 VS Code 插件支持在弹窗中勾选复制当前账号的会话。CodeBuddy IDE 首次使用前需先手动打开并登录一次
5. **管理关联会话**：侧栏「关联会话」按客户端查看同一会话在各账号中的副本状态，把新增内容增量同步到目标账号、处理内容分歧；「新增关联会话」可把会话复制到新账号并建立关联
6. **开关各端入口**：设置 →「支持工具」可按客户端逐个开启 / 关闭入口；关闭后该端在账号页隐藏、不再轮询状态，不影响账号库。JetBrains 端默认关闭
7. **自动轮换**：设置 → CodeBuddy CLI 自动轮换，开启后按积分紧迫程度自动设置默认账号
8. **更新**：应用会自动检查公开 GitHub Releases；发现新版本后可在左下角直接升级，也可从设置页打开 Release 页面手动下载

## 界面预览

### 管理 WorkBuddy 与 CodeBuddy 账号

账号卡片集中展示登录状态、积分余额和到期资源，临期积分直接标注在对应卡片内，并按紧迫程度优先排列。

![账号管理页面（账号信息已脱敏）](docs/images/accounts-overview.png)

### 关联会话

复制到其他账号的会话会自动建立关联：按客户端（WorkBuddy / CodeBuddy IDE / CodeBuddy 插件）集中展示同一会话在各账号中的副本状态，可把来源账号的新增内容增量同步到目标账号；出现内容分歧时选择要保留的一份，也可把会话复制到新账号并建立关联。

![关联会话页面](docs/images/session-links.png)

打开任一关联组可查看「会话关联图」：同一会话在各账号中的副本以分支图呈现，内容分歧时标出共同旧版与各自的独立更新，选定要保留的一份即可统一到其他账号。

![关联会话详情：内容分歧时的分支图](docs/images/session-links-branches.png)

### 积分统计

积分统计页展示官方请求用量、每日趋势、模型分布、账号消耗和请求明细，数据来源与更新时间会明确显示。

![积分统计页面](docs/images/credit-statistics.png)

### Token 统计

Token 统计页按来源展示 Token 总览与趋势、构成占比、活跃热力图、项目/模型 Top 10 与会话排行。

![Token 统计页面](docs/images/token-statistics.png)

### 自动轮换策略

自动轮换的目标是防止积分过期浪费：后台定时查询所有账号的积分到期情况，把 CodeBuddy CLI 的默认账号设为「最紧迫」的账号（最早到期且仍有剩余积分）。为避免默认账号频繁变化，每次检查按以下顺序决策：

七步决策顺序（有效账号 → 紧迫度 → 已是目标 → 冷却期 → 存活门控 → 价值过滤 → 防抖动）、生效边界与全部配置项见 **[使用细节 · 自动轮换策略](docs/usage-details.md#自动轮换策略)**。

## macOS 权限说明

切换账号需要写入 WorkBuddy 认证文件，macOS 要求授权「App 管理」（或「完全磁盘访问」）：

1. 首次切换报「无权限」时，点「打开系统设置」
2. 优先在 **App 管理** 里打开 workbuddy-switch 开关；若没有，则去 **完全磁盘访问** 把 workbuddy-switch 拖进带箭头的框
3. 授权后重启本应用生效；设置页「权限检测」可随时验证

## npm / webui 版本

```bash
npm i -g workbuddy-switch
workbuddy-switch              # 启动本地服务 + 自动打开浏览器
workbuddy-switch status       # 终端查看当前账号
```

界面与桌面 App 一致，功能覆盖上方全部模块，但不提供会话悬浮窗（桌面版专属）。webui 模式下的 macOS 权限由启动服务的终端进程决定；若终端已授权完全磁盘访问则无需额外操作。

### 账号边界说明

切换账号只改变登录态与本地数据归属，以下内容**按账号隔离、不会跟随切换，也不参与数据对齐**（防串号，属有意设计）：

1. **连接器绑定**：连接器按账号各绑一次（`~/.workbuddy/connectors/<uid>/`）。切到未绑定该连接器的账号后，对应能力不可用，需在新账号下重新绑定。
2. **渠道身份与凭据**：微信机器人 token、应用密钥、`credentials/` 等凭据按账号隔离，对齐时命中敏感键即跳过。
3. **云端数据**：云端会话历史与云端自动化副本由服务端持有，本机切号不改云端归属；客户端对云端自动化副本只读，删除须在手机端或小程序操作。

与本机共享层（对话 jsonl、`MEMORY.md`、skills、`mcp.json`、workspaces / tasks）无关，切换账号后依然可见。

### ⚠️ 云端归属错位：切号后别在错误账号下删会话

**会话归属对齐只改本机显示层，不改云端通道归属**——两者一旦不一致，删除动作会双向卡死：手机端（原账号）看得见却删不掉，本机（当前账号）已经没有它、无处可删。

完整复现、根因与四步恢复流程见 **[使用细节 · 云端归属错位](docs/usage-details.md#云端归属错位)**。

**预防**：删除会话前先确认它**当初是在哪个账号下创建的**（看手机端能否在某个账号下看到它）。归属被对齐改写过的会话，不要在当前账号下直接删。

## 致谢

感谢 [Linux.do](https://linux.do) 社区。

## 许可

[MIT](./LICENSE)
