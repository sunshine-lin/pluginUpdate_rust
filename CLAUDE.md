# CLAUDE.md

本文件为 Claude Code 在本仓库工作时提供指引。

## 项目背景

被更新的插件是 **AIChat** —— CJ Dropshipping 面向**采购人员**的 Chrome 扩展（WXT + Vue 3，MV3），
在 1688、CJ PMS 等平台做采购自动化：信息收集、供应商自动沟通、询价归档、AI 找货任务。
插件源码在姊妹仓库 `/Users/sunshine_lin/cj/pms-aichat`。

**本项目（桌面更新器）为什么存在：**

1. AIChat 使用 Chrome Debugger API / CDP 模拟真实鼠标操作绕过 1688 反爬，**无法上架 Chrome 商店**
2. 不上架 ⇒ 没有 Chrome 原生自动更新，必须自建分发链路（下载 zip → 解压到本地目录加载）
3. 采购同事**在虚拟机上办公、通常只有 C 盘** ⇒ 必须支持自定义安装路径并持久化
4. 采购是**非技术人员**，不会手动下载解压、去 `chrome://extensions` 重新加载

⇒ 本项目定位：**面向非技术采购同事的一键更新器**。
交互目标是「点一下就好」：下载 → 解压 → 刷新所有 Chrome 标签页 → 自动打开侧边栏，全程不碰浏览器设置。
**任何改动都应服务于这个目标：能自动做的不要让用户做，报错信息要让非技术人员看得懂。**

> 架构细节、Tauri 命令清单、业务链路见 @agents.md，本文件不重复。

## 关键路径

- `src-tauri/src/lib.rs` — 全部业务逻辑（纯函数 + Tauri 命令 + 内联测试），约 850 行
- `src/App.tsx` — 全部前端 UI 逻辑
- `src-tauri/capabilities/default.json` — Tauri 权限声明
- `docs/需求提示词文档.md` — 需求主文档

## 开发流程

- **直接在 `master` 上开发**：本项目目前单人开发，不新建功能分支。改动直接在 `master` 上
  commit、直接 push 到 `origin/master`，不需要「先推功能分支备份」那一步。
  （`release` 仍是线上分支，验证通过后从 `master` cherry-pick 具体提交号过去。）
- **TDD 是硬要求**：先写 `#[cfg(test)]` 测试再写实现，不允许跳过。测试集中在 `lib.rs` 底部测试模块（当前 18 个）。
- **每完成一个任务立即 `git commit`**，不积攒改动。

## 代码约定

### 纯函数与副作用分离（沿用现有模式）

平台相关操作拆三层，便于单测：

```rust
pub fn build_xxx_script_macos(...) -> String   // 纯函数：构建命令字符串，可测
fn run_xxx_os(...) -> Result<String, String>   // 执行层：#[cfg(target_os)] 分支
#[tauri::command] fn xxx(...)                  // 命令层：校验入参后调用执行层
```

### 平台分支必须写三个

`#[cfg(target_os = "macos")]` / `#[cfg(target_os = "windows")]` /
`#[cfg(not(any(...)))]` —— 最后的 fallback 分支不可省略，否则其他平台编译失败。

### 新增 Tauri 命令要同步改两处

1. `lib.rs` 的 `invoke_handler![]` 注册
2. `capabilities/default.json` 加权限（若用到新插件能力）

漏掉任一处，前端 `invoke` 会静默失败。

## 构建与验证

```bash
cd src-tauri && cargo test    # Rust 单测（改 lib.rs 必跑）
npx tsc --noEmit              # 前端类型检查（改 App.tsx 必跑）
npm run build:online          # 线上包
npm run build:test            # 测试环境包
```

> Node 需 >= 20.19 或 >= 22.12。本机多版本时：
> `export PATH="/usr/local/n/versions/node/22.22.1/bin:$PATH"`
>
> Rust 需 >= 1.95（sysinfo 0.39 要求，`rustup update stable` 升级）。

## 安全红线（改相关代码前必读）

| 红线 | 位置 | 原因 |
|------|------|------|
| 扩展 ID 必须过 `validate_extension_id()`（32 位 a-p） | `lib.rs:121` | 直接拼进 AppleScript/PowerShell，不校验即命令注入 |
| ZIP 解压跳过含 `..` 的条目 | `perform_update()` | 防路径穿越写出安装目录 |
| 仅测试环境禁用 SSL 校验 + 绕过代理 | `build_http_client()` | 线上环境不得关闭校验 |

## 不要做的事

- 不在 `release` 分支直接改代码或提交（`release` = 线上分支）
- 不为图省事绕过扩展 ID 校验
- 不把 agents.md 的架构内容复制到本文件（会漂移）
