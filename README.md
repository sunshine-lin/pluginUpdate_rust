# AIChat 插件更新工具

基于 Tauri (Rust + React) 的桌面应用程序，用于下载和更新 AIChat 插件。

## 功能特点

- 一键检查并更新 AIChat 插件
- 自动对比本地版本与线上版本
- 支持测试环境 (test) 和线上环境 (online) 两种模式
- 支持 macOS 和 Windows 双平台打包
- 自动下载 ZIP 并解压到指定安装路径

## 环境变量

| 环境 | 环境变量 | 下载地址 | 安装路径 (Windows) | 安装路径 (macOS) |
|------|---------|---------|-------------------|-----------------|
| 线上 (默认) | `APP_ENV=online` | https://chainai.cjdropshipping.cn/AIChat.zip | `D:\AIChat` | `~/aichat` |
| 测试 | `APP_ENV=test` | https://cj-chain-ai.cjdropshipping.offline.pre.cn/AIChat.zip | `D:\AIChat_test` | `~/aichat_test` |

## 服务器网址

- **线上环境**: https://chainai.cjdropshipping.cn/AIChat.zip
- **测试环境**: https://cj-chain-ai.cjdropshipping.offline.pre.cn/AIChat.zip
- **版本检查 (线上)**: https://chainai.cjdropshipping.cn/manifest.json
- **版本检查 (测试)**: https://cj-chain-ai.cjdropshipping.offline.pre.cn/manifest.json

## 解压后文件放置位置

| 操作系统 | 线上环境路径 | 测试环境路径 |
|---------|------------|------------|
| Windows | `D:\AIChat\` | `D:\AIChat_test\` |
| macOS | `~/aichat/` | `~/aichat_test/` |

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
# 线上环境(默认)
npm run tauri dev

# 测试环境
APP_ENV=test npm run tauri dev
```

### 打包构建

#### macOS 打包

```bash
# 线上环境
npm run tauri build

# 测试环境
APP_ENV=test npm run tauri build
```

#### Windows 打包 (交叉编译)

需要在 Windows 系统上执行：

```bash
# 线上环境
npm run tauri build

# 测试环境
set APP_ENV=test && npm run tauri build
```

### 打包输出

打包后的应用程序位于：
- macOS: `src-tauri/target/release/bundle/dmg/` (DMG 安装包)
- Windows: `src-tauri/target/release/bundle/msi/` (MSI 安装包) 或 `nsis/` (EXE 安装包)

## 使用说明

1. 启动应用后，界面会显示当前环境、版本号和安装路径
2. 点击 **"🔄 立即检查更新"** 按钮检查是否有新版本
3. 如果有新版本，会弹出确认框，点击"确定更新"开始下载安装
4. 更新完成后会自动刷新版本信息

## 项目结构

```
pluginUpdate_rust/
├── src/                   # React 前端代码
│   ├── App.tsx            # 主界面组件
│   ├── App.css            # 样式文件
│   └── main.tsx           # 入口文件
├── src-tauri/             # Rust 后端代码
│   ├── src/
│   │   ├── lib.rs         # 核心逻辑(下载、解压、版本对比)
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
