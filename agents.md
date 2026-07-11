# aichat 插件更新工具 — 项目架构说明（agents.md）

> 生成时间：26年04月02日 11:03:xx | 项目根目录：/Users/sunshine_lin/cj/pluginUpdate_rust

---

## 1. 项目定位

桌面端 Tauri v2 应用，用于检测并自动安装 **aichat** Chrome 浏览器插件的更新包。  
支持线上/测试双环境切换、自定义安装路径持久化、更新后自动刷新 Chrome 标签页及打开插件侧边栏。

---

## 2. 技术栈

| 层次 | 技术 |
|------|------|
| 前端 UI | React 19 + TypeScript + Vite 7 |
| 桌面框架 | Tauri v2 |
| 后端逻辑 | Rust 2021 edition |
| HTTP 客户端 | reqwest 0.12（async） |
| 序列化 | serde + serde_json |
| 文件解压 | zip 2 |
| 临时目录 | tempfile 3 |
| 目录解析 | dirs 6 |
| 对话框插件 | tauri-plugin-dialog 2 |
| 文件打开 | tauri-plugin-opener 2 |

---

## 3. 目录结构

```
pluginUpdate_rust/
├── src/                       # 前端 React 源码
│   ├── App.tsx                # 主应用组件（所有 UI 逻辑）
│   ├── App.css                # 组件样式
│   └── main.tsx               # 前端入口
├── src-tauri/                 # Rust 后端
│   ├── src/
│   │   ├── lib.rs             # 所有 Tauri 命令、纯函数、TDD 测试
│   │   └── main.rs            # Rust 入口（调用 lib::run()）
│   ├── Cargo.toml             # Rust 依赖配置
│   ├── tauri.conf.json        # 线上打包配置
│   ├── tauri.test.conf.json   # 测试环境打包配置
│   └── capabilities/
│       └── default.json       # Tauri 权限（含 dialog:default）
├── docs/
│   ├── 需求提示词文档.md        # 主任务文档（阅读标记在此）
│   └── 需求/                  # 子需求文档（按模块拆分）
├── agents.md                  # 本文件（项目架构说明）
├── index.html                 # Tauri 前端入口 HTML
├── vite.config.ts
├── tsconfig.json
└── package.json
```

---

## 4. 核心业务链路

### 4.1 更新检查与执行

```
前端 handleCheckUpdate()
  → invoke("check_update", { env })
    → Rust: check_update(env)
      → get_install_path(env)              # 优先缓存路径 > 默认路径
      → get_local_version(install_path)    # 读取 manifest.json
      → HTTP GET manifest_url              # 获取远端版本
      → version_compare(remote, local)     # 比较版本
  ← CheckResult { has_update, current_version, remote_version, install_path }

如有更新 → 前端显示确认对话框 → handleConfirmUpdate()
  → invoke("perform_update", { env })
    → 下载 aichat.zip → 解压到 install_path
  ← "更新完成！当前版本: x.x.x"
  → postUpdateChromeActions()
    → invoke("refresh_chrome_tabs")        # 刷新所有 Chrome 标签页
    → invoke("open_chrome_sidebar", { extensionId })  # 打开侧边栏（已配置时）
```

### 4.2 自定义安装路径

```
前端 handleEditPath() / handleBrowsePath()
  → 用户输入或系统目录选择对话框（tauri-plugin-dialog）
  → invoke("save_custom_path", { env, path })
    → Rust: save_path_to_config_file(config_file, env, path)
      → 读取已有 config.json（保留另一 env 和 extension_id）
      → 更新对应 env 路径字段
      → 写回 config.json

下次加载:
  → invoke("get_app_info", { env })
    → Rust: get_install_path(env)
      → load_saved_path_from_file(config_file, env)
      → 无则 get_install_path_resolved(env, None) → 默认路径
```

### 4.3 配置文件结构（~/.config/aichat-updater/config.json）

```json
{
  "online_path": "/Users/xxx/aichat",
  "test_path": "/custom/aichat_test",
  "extension_id": "abcdefghijklmnopabcdefghijklmnop"
}
```

---

## 5. Tauri 命令清单

| 命令 | 入参 | 返回 | 说明 |
|------|------|------|------|
| `get_app_info` | `env: String` | `UpdateInfo` | 获取本地版本+路径+下载URL |
| `check_update` | `env: String` | `Result<CheckResult>` | 对比本地与远端版本 |
| `perform_update` | `env: String` | `Result<String>` | 下载并解压更新包 |
| `get_saved_path` | `env: String` | `Option<String>` | 读取缓存路径 |
| `save_custom_path` | `env, path: String` | `Result<()>` | 持久化自定义路径 |
| `get_extension_id` | — | `Option<String>` | 读取 Chrome 扩展 ID |
| `save_extension_id` | `id: String` | `Result<()>` | 持久化扩展 ID（32位a-p） |
| `open_chrome_sidebar` | `extensionId: String` | `Result<String>` | RPA 打开侧边栏 |
| `refresh_chrome_tabs` | — | `Result<String>` | 刷新所有 Chrome 标签页 |

---

## 6. TDD 测试覆盖（lib.rs #[cfg(test)]）

共 14 个单元测试，覆盖：
- 配置文件路径拼接
- 路径写入/读取/覆盖/多环境隔离
- 安装路径回退逻辑（自定义 > 默认）
- 配置 JSON 格式合法性
- 扩展 ID 格式校验（防注入）
- 扩展 ID 存读/不覆盖路径
- Chrome 侧边栏命令构建（macOS/Windows）
- Chrome 标签页刷新脚本构建（macOS/Windows）

运行命令：
```bash
cd src-tauri && cargo test
```

---

## 7. 构建命令

```bash
# 线上版本
npm run tauri build
# 测试环境版本
npm run build:test
# 仅前端编译校验
npx tsc --noEmit
```

---

## 8. 环境区分

| 环境 | 下载地址 | 默认安装路径 |
|------|---------|------------|
| online | `https://chainai.cjdropshipping.cn/aichat.zip` | `~/aichat` / `D:\aichat` |
| test | `https://cj-chain-ai.cjdropshipping.offline.pre.cn/aichat.zip` | `~/aichat_test` / `D:\aichat_test` |

---

## 9. 安全说明

- 扩展 ID 严格校验（32位 a-p）防止 AppleScript/PowerShell 命令注入
- ZIP 解压跳过含 `..` 的路径（防路径穿越攻击）
- 测试环境禁用 SSL 证书校验 + 绕过系统代理
