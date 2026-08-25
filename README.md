# aichat 插件更新工具

基于 Tauri (Rust + React) 的桌面应用程序，用于下载和更新 aichat 插件。

## 功能特点

- 一键检查并更新 aichat 插件
- 自动对比本地版本与线上版本
- 单安装包支持两种环境：线上 (online) 与测试 (test)
- 顶部 Tab 运行时切换环境，无需重新安装不同版本客户端
- 测试环境自动禁用 SSL 证书验证并绕过系统代理，兼容内网 HTTPS 地址
- 支持 macOS 和 Windows 双平台打包
- 自动下载 ZIP 并解压到指定安装路径
- **支持自定义安装路径**：UI 中点击「✏️ 修改」可输入自定义路径或通过系统文件夹选择器浏览，路径持久化保存、重启自动恢复
- **路径缓存**：线上/测试环境独立缓存，优先使用上次保存的自定义路径，无则回退默认路径
- **更新后自动刷新 Chrome**：更新完成后自动刷新所有 Chrome 窗口的全部标签页（macOS AppleScript / Windows PowerShell）
- **RPA 打开侧边栏**：配置 Chrome 扩展 ID 后，更新完成自动通过 RPA 打开 aichat 扩展侧边栏，也支持手动点击打开
- **机器状态查看**：独立 Tab 展示当前电脑的内存、CPU（型号/核数/实时占用率）、安装目录所在磁盘的可用空间、系统版本，每 3 秒自动刷新，辅助判断是否因机器资源紧张导致卡顿

## 安装路径说明

| 平台 | 线上默认 | 测试默认 | 说明 |
|------|---------|---------|------|
| Windows | `D:\aichat` | `D:\aichat_test` | 可修改为 `C:\aichat` 等任意路径 |
| macOS | `~/aichat` | `~/aichat_test` | 可在 UI 中修改 |

> **自定义路径配置文件位置：**
> - macOS: `~/Library/Application Support/aichat-updater/config.json`
> - Windows: `%APPDATA%\aichat-updater\config.json`

## 环境与地址

| 环境 | 下载地址 | 安装路径 (Windows) | 安装路径 (macOS) |
|------|---------|-------------------|------------------|
| 线上 | https://chainai.cjdropshipping.cn/aichat.zip | `D:\aichat`（可自定义） | `~/aichat`（可自定义） |
| 测试 | https://cj-chain-ai.cjdropshipping.offline.pre.cn/aichat.zip | `D:\aichat_test`（可自定义） | `~/aichat_test`（可自定义） |

## 服务器网址

- **线上环境**: https://chainai.cjdropshipping.cn/aichat.zip
- **测试环境**: https://cj-chain-ai.cjdropshipping.offline.pre.cn/aichat.zip
- **版本检查 (线上)**: https://chainai.cjdropshipping.cn/manifest.json
- **版本检查 (测试)**: https://cj-chain-ai.cjdropshipping.offline.pre.cn/manifest.json

## 解压后文件放置位置

| 操作系统 | 线上环境路径 | 测试环境路径 |
|---------|------------|------------|
| Windows | 自定义路径（默认 `D:\aichat\`） | 自定义路径（默认 `D:\aichat_test\`） |
| macOS | 自定义路径（默认 `~/aichat/`） | 自定义路径（默认 `~/aichat_test/`） |

## 开发环境要求

- Node.js >= 20.19 或 >= 22.12（Vite 7 要求，推荐使用 `n` 或 `nvm` 管理版本）
- Rust >= 1.95（sysinfo 0.39 要求；低于此版本 `cargo build` 会直接报错拒绝编译）
- npm >= 8

> macOS 本机若有多版本 Node，构建前可临时切换：`export PATH="/usr/local/n/versions/node/22.22.1/bin:$PATH"`

## 快速开始

### 安装依赖

```bash
npm install
```

### 开发模式

```bash
npm run tauri dev
```

### 打包构建

统一构建一个安装包，运行时通过顶部 Tab 切换线上/测试环境：

```bash
npm run tauri build
```

### 打包输出

打包后的应用程序位于：
- macOS: `src-tauri/target/release/bundle/dmg/` (DMG 安装包)
- Windows: `src-tauri/target/release/bundle/msi/` (MSI 安装包) 或 `nsis/` (EXE 安装包)

## 使用说明

1. 启动应用后，在顶部 Tab 选择目标环境（线上/测试）
2. 安装路径行右侧点击 **「✏️ 修改」** 可自定义安装目录
   - 直接输入路径（如 `C:\aichat`）或点击 **「📁」** 使用系统文件夹选择器
   - 点击 **「✅」** 保存，路径持久化到本地配置文件
3. 点击 **「🔄 立即检查更新」** 按钮检查是否有新版本
4. 如果有新版本，会弹出确认框，点击「确定更新」开始下载安装
5. 更新完成后自动刷新所有 Chrome 标签页，并打开已配置的 aichat 侧边栏
6. 在 **「Chrome 扩展 ID」** 配置区输入 32 位扩展 ID（格式：a-p 小写字母），可手动点击「打开侧边栏」

## 远程查插件日志（aichat-logs 技能）

排查采购插件问题时，**给一个插件实例名就能拿到它的日志** —— 不用知道那个实例
在哪台采购机、IP 是多少，也不用登到机器上去翻文件。

### 安装（一次，约 1 分钟）

```bash
bash skills/aichat-logs/install.sh
```

装到 `~/.claude/skills/aichat-logs/`（**用户级**），所以在**任何项目**下都能用 ——
排查时日志在本项目、插件代码在 `pms-aichat`，装成项目级就只有一个能用、
等于逼人来回切目录。

安装脚本会自动扫局域网找机器、建立「插件名 → 机器」索引。

### 怎么用：直接跟 Claude Code 说人话

```
「查一下 10-1-LS10005 的日志」
「虚拟机3-chrome7-cj07002 今天报了什么错」
「8-14-LS10002 为什么不跑任务」
「10 号机今天异常最多的是哪个实例」
```

不需要说「用 aichat-logs 技能」「连到 192.168.x.x」——技能描述已覆盖这些场景词。

也可以直接跑命令：

```bash
S=~/.claude/skills/aichat-logs/aichat_logs.py

python3 $S --plugin 10-1-LS10005 --date 2026-08-25 --level ERROR
python3 $S --plugin 虚拟机3-chrome7-cj07002 --summary   # 聚合概览，先看全局
python3 $S --list-plugins        # 全部实例分别在哪台机器
python3 $S --scan                # 重扫机器（IP 变动后；平时不用手动跑）
python3 $S --rebuild-index       # 重建实例索引（实例增删后）
```

### 前提

| | |
|---|---|
| Python | 3.8+（macOS 自带；Windows 需自行安装） |
| 网络 | **在公司局域网内**（含 VPN）。扫描范围 `192.168.0.x ~ 192.168.7.x` |
| 目标机器 | 客户端 ≥ 0.3.0，且已放行 **17653** 入站端口 |
| 凭证 | 不需要 —— 也就是说**局域网内谁都能读**，日志没有访问控制 |

端口没放行的机器扫不到。在那台机器上以管理员身份执行一次即可：

```cmd
netsh advfirewall firewall add rule name="aichat-updater" dir=in action=allow protocol=TCP localport=17653
```

### 查询顺序：先看全局再收窄

单台一天可达 **13 万行**（10 台合计约 130 万行/天），不要一上来拉全量：

```bash
python3 $S --plugin <任一实例名> --summary --date 2026-08-25   # 1. 哪个实例异常最多、主要是哪几类错
python3 $S --plugin <定位到的实例> --level ERROR --from 09:00 --to 12:00  # 2. 再收窄
```

`--summary` 是全量扫描，但聚合在**那台机器本地**完成，只把统计结果传回来。

### 两个容易踩的坑

- **`total: 0` 不是结论** —— 日志只保留 **7 天**，查更早的日期必然为空，
  这**不代表当时没问题**。先用 `--dates` 确认有哪些日期，再查实例名拼写与时间段。
- **实例名有三种命名风格**：`虚拟机N-chromeM-账号`、`N-M-账号`、`虚拟机N-M-账号`，
  都支持。拼不准就先 `--list-plugins` 看索引。

### 数据敏感性

日志含**供应商聊天内容与采购报价**。token/密码/手机号已在写入前脱敏（插件侧
`sanitizeSensitiveInfo`），但**业务数据没脱** —— 脱了就没法排查。

日志**始终留在那台采购机本地**，远程模式只是读、不复制到别处。不要把日志原文
粘到外部服务；汇报时引用具体错误信息与统计结论即可。

> 文档与代码里的示例账号 ID（`LS100xx` / `cj070xx`）都是**占位符**，不是真实账号 ——
> 本仓库有公开的 GitHub 远端。真实实例名跑 `--list-plugins` 看本机索引。
> 两个本机缓存（`machines.json` / `plugin-index.json`）同理不入库，装的时候自动重建。

## 项目结构

```
pluginUpdate_rust/
├── src/                   # React 前端代码
│   ├── App.tsx            # 主界面组件
│   ├── App.css            # 样式文件
│   └── main.tsx           # 入口文件
├── src-tauri/             # Rust 后端代码
│   ├── src/
│   │   ├── lib.rs         # 核心逻辑(下载、解压、版本对比、自定义路径持久化)
│   │   └── main.rs        # 程序入口
│   ├── Cargo.toml         # Rust 依赖配置
│   ├── tauri.conf.json    # Tauri 配置
│   └── icons/             # 应用图标
├── docs/                  # 需求文档
├── scripts/               # 构建脚本
├── package.json           # 前端依赖配置
└── README.md              # 本文档
```

## 技术栈

- **桌面框架**: Tauri 2
- **前端**: React 19 + TypeScript + Vite
- **后端**: Rust
- **HTTP 客户端**: reqwest
- **ZIP 处理**: zip crate
- **序列化**: serde + serde_json
- **目录解析**: dirs
- **对话框**: tauri-plugin-dialog

## 运行测试

```bash
cd src-tauri && cargo test
```

当前共 18 个单元测试，覆盖路径持久化、安装路径解析、扩展 ID 校验（防注入）、Chrome 脚本构建等核心逻辑。
