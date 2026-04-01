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

- Node.js >= 18
- Rust >= 1.70
- npm >= 8

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
5. 更新完成后会自动刷新当前 Tab 对应环境的版本信息

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
