use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use tauri::{Emitter, Manager};

pub mod heartbeat;
pub mod log_file;
pub mod log_server;
pub mod updater_manifest;
pub mod ws_token;

#[derive(Debug, Serialize, Deserialize)]
struct UpdateInfo {
    install_path: String,
    current_version: String,
    env: String,
    download_url: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct CheckResult {
    has_update: bool,
    current_version: String,
    remote_version: String,
    install_path: String,
}

/// 配置文件结构：记录自定义安装路径（区分 online/test 环境）
#[derive(Debug, Default, Serialize, Deserialize)]
struct PathConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    online_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    test_path: Option<String>,
    /// 开机自启偏好。默认关闭：装上不改变用户开机行为，由用户在托盘菜单主动开启
    #[serde(skip_serializing_if = "Option::is_none")]
    autostart: Option<bool>,
    /// 透传本结构未声明的字段。serde 默认丢弃未知字段，
    /// 若不保留会在写入任一配置项时静默抹掉其它程序写入的数据
    #[serde(flatten)]
    extra: std::collections::BTreeMap<String, serde_json::Value>,
}

// ─────────────────────────────────────────────────────────────────
// 路径持久化核心函数（可测试的纯函数，接受注入目录）
// ─────────────────────────────────────────────────────────────────

/// 根据给定目录拼接配置文件路径（config.json）
/// # Arguments
/// * `config_dir` - 配置文件所在目录
pub fn get_config_file_path_with_dir(config_dir: &PathBuf) -> PathBuf {
    config_dir.join("config.json")
}

/// 从指定配置文件加载对应 env 的自定义路径
/// 文件不存在或解析失败时返回 None，不会 panic
/// # Arguments
/// * `config_file` - 配置文件完整路径
/// * `env` - 环境标识 "online" 或 "test"
pub fn load_saved_path_from_file(config_file: &PathBuf, env: &str) -> Option<String> {
    let content = fs::read_to_string(config_file).ok()?;
    let cfg: PathConfig = serde_json::from_str(&content).ok()?;
    match env {
        "test" => cfg.test_path,
        _ => cfg.online_path,
    }
}

/// 将自定义路径写入指定配置文件（覆盖同 env 的旧值，保留其他 env 的值）
/// # Arguments
/// * `config_file` - 配置文件完整路径
/// * `env` - 环境标识
/// * `path` - 要保存的路径
pub fn save_path_to_config_file(
    config_file: &PathBuf,
    env: &str,
    path: &str,
) -> Result<(), String> {
    // 先尝试读取已有配置，避免覆盖另一个 env 的路径
    let mut cfg: PathConfig = if config_file.exists() {
        fs::read_to_string(config_file)
            .ok()
            .and_then(|c| serde_json::from_str(&c).ok())
            .unwrap_or_default()
    } else {
        PathConfig::default()
    };

    match env {
        "test" => cfg.test_path = Some(path.to_string()),
        _ => cfg.online_path = Some(path.to_string()),
    }

    if let Some(parent) = config_file.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("创建配置目录失败: {}", e))?;
    }

    let content = serde_json::to_string_pretty(&cfg)
        .map_err(|e| format!("序列化配置失败: {}", e))?;
    fs::write(config_file, content).map_err(|e| format!("写入配置文件失败: {}", e))
}

/// 根据自定义路径或默认路径解析最终安装路径（受测试覆盖的核心逻辑）
/// # Arguments
/// * `env` - 环境标识
/// * `custom_path` - 用户指定的自定义路径（优先使用）
pub fn get_install_path_resolved(env: &str, custom_path: Option<String>) -> PathBuf {
    if let Some(p) = custom_path {
        if !p.trim().is_empty() {
            return PathBuf::from(p);
        }
    }
    // 无自定义路径时使用默认值
    let folder_name = if env == "test" { "aichat_test" } else { "aichat" };
    if cfg!(target_os = "windows") {
        PathBuf::from("D:\\").join(folder_name)
    } else {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join(folder_name)
    }
}

/// 从配置文件加载开机自启偏好。
/// 配置缺失/解析失败时返回 false —— 默认不自启，避免未经用户同意修改开机项
pub fn load_autostart_from_file(config_file: &PathBuf) -> bool {
    fs::read_to_string(config_file)
        .ok()
        .and_then(|c| serde_json::from_str::<PathConfig>(&c).ok())
        .and_then(|cfg| cfg.autostart)
        .unwrap_or(false)
}

/// 将开机自启偏好写入配置文件（保留其他字段不变）
/// # Arguments
/// * `config_file` - 配置文件路径
/// * `enabled` - 是否开机自启
pub fn save_autostart_to_file(config_file: &PathBuf, enabled: bool) -> Result<(), String> {
    let mut cfg: PathConfig = if config_file.exists() {
        fs::read_to_string(config_file)
            .ok()
            .and_then(|c| serde_json::from_str(&c).ok())
            .unwrap_or_default()
    } else {
        PathConfig::default()
    };
    cfg.autostart = Some(enabled);
    if let Some(parent) = config_file.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("创建配置目录失败: {}", e))?;
    }
    let content = serde_json::to_string_pretty(&cfg)
        .map_err(|e| format!("序列化配置失败: {}", e))?;
    fs::write(config_file, content).map_err(|e| format!("写入配置文件失败: {}", e))
}

// ─────────────────────────────────────────────────────────────────
// 日志格式化纯函数（Task 1.2/1.3，可测试）
// ─────────────────────────────────────────────────────────────────

/// 将插件传入的日志级别归一为固定大写形式。
/// 未知级别回落 INFO 而非丢弃，避免插件传入非约定值时日志静默消失
pub fn normalize_log_level(level: &str) -> &'static str {
    match level.trim().to_ascii_lowercase().as_str() {
        "error" | "err" => "ERROR",
        "warn" | "warning" => "WARN",
        "debug" => "DEBUG",
        _ => "INFO",
    }
}

/// 判断该级别是否应额外写入异常日志文件（供 AI 分析用，Task 2.3）
pub fn is_error_level(level: &str) -> bool {
    matches!(normalize_log_level(level), "ERROR" | "WARN")
}

/// 格式化单条日志为一行文本：`[时间] [级别] [来源] [插件名] 消息`
/// 插件名标识具体是哪台机器/哪个采购账号（见插件侧 PLUGIN_NAME），
/// 多台机器的日志汇总后靠这个字段区分来源。消息内的换行/回车会被转义，
/// 保证「一行一条」，否则 grep 与 AI 解析会错乱
pub fn format_log_line(
    timestamp: &str,
    level: &str,
    source: &str,
    plugin_name: &str,
    message: &str,
) -> String {
    let flat = message.replace('\r', "").replace('\n', "\\n");
    format!(
        "[{}] [{}] [{}] [{}] {}",
        timestamp,
        normalize_log_level(level),
        source,
        plugin_name,
        flat
    )
}

/// 当前本地时间戳，格式 `YYYY-MM-DD HH:MM:SS.mmm`。
/// 插件未提供 timestamp 时由服务端补齐
pub fn current_timestamp() -> String {
    chrono::Local::now()
        .format("%Y-%m-%d %H:%M:%S%.3f")
        .to_string()
}

/// 把插件上报的时间戳归一为本地时间（北京时间），格式 `YYYY-MM-DD HH:MM:SS.mmm`。
///
/// 插件侧用 `new Date().toISOString()` 上报，得到的是 **UTC** 串
/// （如 `2026-08-18T01:25:18.602Z`）。若原样落盘，日志页显示的时刻比实际
/// 早 8 小时，排查者会按错误的时间去找现场；`T`/`Z` 这类 ISO 记法对
/// 采购同事也不可读。
///
/// 归一在**服务端入库时**做而非前端展示时做：日志文件本身就该是可直接阅读的
/// 北京时间，前端只是消费方之一（还有人直接翻日志目录里的文件）。
///
/// 认不出格式时原样返回——日志内容比格式统一更重要，不能因解析失败丢掉时间信息。
pub fn normalize_timestamp(raw: &str) -> String {
    // 带时区的 ISO8601（Z 或 ±HH:MM）：转成本地时区
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(raw) {
        return dt
            .with_timezone(&chrono::Local)
            .format("%Y-%m-%d %H:%M:%S%.3f")
            .to_string();
    }
    // 已是本地格式（服务端自己生成的）或无法识别：原样返回，
    // 不做二次偏移——否则每经一次处理就多加 8 小时
    raw.to_string()
}

// ─────────────────────────────────────────────────────────────────
// Chrome 标签页刷新相关纯函数（可测试）
// ─────────────────────────────────────────────────────────────────

/// 构建 macOS AppleScript：刷新所有 Chrome 窗口的所有标签页
pub fn build_refresh_all_tabs_script_macos() -> String {
    "tell application \"Google Chrome\"\n\
     if running then\n\
       repeat with w in windows\n\
         repeat with t in tabs of w\n\
           reload t\n\
         end repeat\n\
       end repeat\n\
     end if\n\
     end tell"
        .to_string()
}

/// 构建 Windows PowerShell 命令：给所有 Chrome 窗口发送 Ctrl+R 刷新快捷键
pub fn build_refresh_all_tabs_command_windows() -> String {
    "Add-Type -AssemblyName System.Windows.Forms; \
     $chrome = Get-Process -Name chrome -ErrorAction SilentlyContinue; \
     if ($chrome) { \
       foreach ($w in (New-Object -ComObject Shell.Application).Windows() | \
         Where-Object { $_.Name -eq 'Google Chrome' }) { \
         $w.Refresh() \
       } \
     }"
    .to_string()
}

/// 执行平台相关的 Chrome 标签页刷新命令
#[cfg(target_os = "macos")]
fn run_refresh_chrome_tabs_os() -> Result<String, String> {
    let script = build_refresh_all_tabs_script_macos();
    let output = std::process::Command::new("osascript")
        .args(["-e", &script])
        .output()
        .map_err(|e| format!("执行AppleScript刷新失败: {}", e))?;
    if output.status.success() {
        Ok("已刷新所有 Chrome 标签页".to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        // Chrome 未运行时非致命，返回 ok
        if stderr.contains("is not running") || stderr.is_empty() {
            Ok("Chrome 未运行，跳过刷新".to_string())
        } else {
            Err(format!("AppleScript刷新失败: {}", stderr))
        }
    }
}

#[cfg(target_os = "windows")]
fn run_refresh_chrome_tabs_os() -> Result<String, String> {
    let cmd = build_refresh_all_tabs_command_windows();
    let output = std::process::Command::new("powershell")
        .args(["-Command", &cmd])
        .output()
        .map_err(|e| format!("执行PowerShell刷新失败: {}", e))?;
    if output.status.success() {
        Ok("已刷新所有 Chrome 标签页".to_string())
    } else {
        Err(format!(
            "PowerShell刷新失败: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn run_refresh_chrome_tabs_os() -> Result<String, String> {
    Err("当前平台不支持自动刷新 Chrome 标签页".to_string())
}

// ─────────────────────────────────────────────────────────────────
// 自动打开插件侧边栏（DEV-124702）
//
// Chrome 规定 sidePanel.open() 必须由**用户手势**触发，外部进程既碰不到
// 该 API、也无法伪造手势——2026-07-30 的 af1233b 曾试过「拼
// chrome-extension://<id>/sidepanel.html 让 Chrome 导航」，那只是在标签页
// 里打开了一个 HTML 文件、并未触发真正的侧边栏，故当时判定「不可靠」而移除。
//
// 本方案改为**模拟按下插件已注册的快捷键**（Ctrl+Shift+L，见插件
// wxt.config.ts 的 commands.toggle_sidepanel，处理逻辑在 background.ts
// 里调 sidePanel.open()）：键盘事件是合法的用户手势，Chrome 会正常触发
// 插件的 commands 监听。区别在于「走合法路径触发真 API」而非「绕过 API
// 模拟效果」。已在 Windows 虚拟机手动验证快捷键可打开侧边栏。
// ─────────────────────────────────────────────────────────────────

/// 构建 Windows PowerShell 命令：激活 Chrome 后发送 Ctrl+Shift+L 打开侧边栏。
///
/// SendKeys 记法：`^` = Ctrl，`+` = Shift，故 `^+l` 即 Ctrl+Shift+L。
/// 必须先 AppActivate——SendKeys 是发给「当前焦点窗口」的，不激活会把按键
/// 发给别的程序；激活是异步的，紧接着发按键会丢失，故中间需要等待。
pub fn build_open_sidepanel_command_windows() -> String {
    "Add-Type -AssemblyName Microsoft.VisualBasic; \
     Add-Type -AssemblyName System.Windows.Forms; \
     $p = Get-Process -Name chrome -ErrorAction SilentlyContinue | \
       Where-Object { $_.MainWindowHandle -ne 0 } | Select-Object -First 1; \
     if ($p) { \
       [Microsoft.VisualBasic.Interaction]::AppActivate($p.Id); \
       Start-Sleep -Milliseconds 400; \
       [System.Windows.Forms.SendKeys]::SendWait('^+l') \
     }"
    .to_string()
}

/// 构建 macOS AppleScript：激活 Chrome 后发送 Command+Shift+L 打开侧边栏。
///
/// 仅供本机开发调试——采购同事全部使用 Windows 虚拟机。
/// macOS 下 System Events 模拟按键需用户授予「辅助功能」权限。
pub fn build_open_sidepanel_script_macos() -> String {
    "tell application \"Google Chrome\" to activate\n\
     delay 0.4\n\
     tell application \"System Events\"\n\
       keystroke \"l\" using {command down, shift down}\n\
     end tell"
        .to_string()
}

/// 执行平台相关的「打开插件侧边栏」命令
#[cfg(target_os = "macos")]
fn run_open_sidepanel_os() -> Result<String, String> {
    let script = build_open_sidepanel_script_macos();
    let output = std::process::Command::new("osascript")
        .args(["-e", &script])
        .output()
        .map_err(|e| format!("执行AppleScript打开侧边栏失败: {}", e))?;
    if output.status.success() {
        Ok("已发送打开侧边栏快捷键".to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        // Chrome 未运行时非致命：自愈流程可能在 Chrome 启动前就跑到这里
        if stderr.contains("is not running") || stderr.is_empty() {
            Ok("Chrome 未运行，跳过打开侧边栏".to_string())
        } else {
            Err(format!("AppleScript打开侧边栏失败: {}", stderr))
        }
    }
}

#[cfg(target_os = "windows")]
fn run_open_sidepanel_os() -> Result<String, String> {
    let cmd = build_open_sidepanel_command_windows();
    let output = std::process::Command::new("powershell")
        .args(["-Command", &cmd])
        .output()
        .map_err(|e| format!("执行PowerShell打开侧边栏失败: {}", e))?;
    if output.status.success() {
        Ok("已发送打开侧边栏快捷键".to_string())
    } else {
        Err(format!(
            "PowerShell打开侧边栏失败: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn run_open_sidepanel_os() -> Result<String, String> {
    Err("当前平台不支持自动打开插件侧边栏".to_string())
}

// ─────────────────────────────────────────────────────────────────
// 重启 Chrome（DEV-124702 二级自愈）
//
// 插件「全死」（Service Worker 崩溃到唤不醒）时收不到 reload 指令，
// 只能重启浏览器让插件随之重新加载——这是代价最大的自愈手段，仅在
// 一级（下发 reload）无效后才升级到此处，且调用方须自行控制冷却期。
// ─────────────────────────────────────────────────────────────────

/// 构建 Windows PowerShell 命令：温和关闭 Chrome，失败则强杀，随后重新拉起。
///
/// 为何先 CloseMainWindow 而非直接 taskkill /F：强杀会被 Chrome 判定为异常
/// 崩溃，下次启动弹「要恢复页面吗」提示，采购同事的标签页也可能丢失。
/// 但温和关闭可能因页面弹「确定要离开吗」而卡住，故留强杀兜底。
/// 关闭与重启之间必须等待——进程未真正退出时启动新实例会复用旧进程，
/// 插件不会重新加载，自愈等于没做。
pub fn build_restart_chrome_command_windows() -> String {
    "$procs = Get-Process -Name chrome -ErrorAction SilentlyContinue; \
     if ($procs) { \
       $procs | ForEach-Object { $_.CloseMainWindow() | Out-Null }; \
       Start-Sleep -Seconds 3; \
       Get-Process -Name chrome -ErrorAction SilentlyContinue | \
         ForEach-Object { $_.Kill() }; \
       Start-Sleep -Seconds 2 \
     }; \
     Start-Process 'chrome'"
        .to_string()
}

/// 构建 macOS AppleScript：退出并重新打开 Chrome。
/// 仅供本机开发调试——采购同事全部使用 Windows 虚拟机
pub fn build_restart_chrome_script_macos() -> String {
    "tell application \"Google Chrome\" to quit\n\
     delay 3\n\
     tell application \"Google Chrome\" to activate"
        .to_string()
}

/// 执行平台相关的「重启 Chrome」命令
#[cfg(target_os = "macos")]
fn run_restart_chrome_os() -> Result<String, String> {
    let script = build_restart_chrome_script_macos();
    let output = std::process::Command::new("osascript")
        .args(["-e", &script])
        .output()
        .map_err(|e| format!("执行AppleScript重启Chrome失败: {}", e))?;
    if output.status.success() {
        Ok("已重启 Chrome".to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        if stderr.contains("is not running") || stderr.is_empty() {
            Ok("Chrome 未运行，已尝试启动".to_string())
        } else {
            Err(format!("AppleScript重启Chrome失败: {}", stderr))
        }
    }
}

#[cfg(target_os = "windows")]
fn run_restart_chrome_os() -> Result<String, String> {
    let cmd = build_restart_chrome_command_windows();
    let output = std::process::Command::new("powershell")
        .args(["-Command", &cmd])
        .output()
        .map_err(|e| format!("执行PowerShell重启Chrome失败: {}", e))?;
    if output.status.success() {
        Ok("已重启 Chrome".to_string())
    } else {
        Err(format!(
            "PowerShell重启Chrome失败: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn run_restart_chrome_os() -> Result<String, String> {
    Err("当前平台不支持重启 Chrome".to_string())
}

// ─────────────────────────────────────────────────────────────────
// 私有辅助函数
// ─────────────────────────────────────────────────────────────────

/// 获取应用配置目录（用于实际运行时，区别于测试注入目录）
fn get_app_config_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| {
            dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
        })
        .join("aichat-updater")
}

/// 获取实际运行时配置文件路径
fn get_runtime_config_file() -> PathBuf {
    get_config_file_path_with_dir(&get_app_config_dir())
}

/// 获取日志目录（Task 1.3 落盘位置，Task 1.1 托盘菜单先用于打开目录）
/// macOS 遵循 ~/Library/Logs 约定，Windows 用 LOCALAPPDATA，其他平台回落配置目录
#[cfg(target_os = "macos")]
fn get_log_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Library/Logs/aichat-updater")
}

#[cfg(target_os = "windows")]
fn get_log_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| get_app_config_dir())
        .join("aichat-updater")
        .join("logs")
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn get_log_dir() -> PathBuf {
    get_app_config_dir().join("logs")
}

/// 根据运行时环境获取下载 URL
fn get_download_url(env: &str) -> String {
    if env == "test" {
        "https://cj-chain-ai.cjdropshipping.offline.pre.cn/aichat.zip".to_string()
    } else {
        "https://chainai.cjdropshipping.cn/aichat.zip".to_string()
    }
}

/// 根据运行时环境获取安装路径（先尝试加载缓存的自定义路径，再回退默认值）
fn get_install_path(env: &str) -> PathBuf {
    let config_file = get_runtime_config_file();
    let custom_path = load_saved_path_from_file(&config_file, env);
    get_install_path_resolved(env, custom_path)
}

/// 构建 HTTP 客户端（测试环境禁用 SSL 验证 + 绕过系统代理）
fn build_http_client(env: &str) -> Result<reqwest::Client, reqwest::Error> {
    let builder = reqwest::Client::builder();
    if env == "test" {
        builder
            .danger_accept_invalid_certs(true)
            .no_proxy() // 绕过系统代理（等同 curl --noproxy "*"）
            .build()
    } else {
        builder.build()
    }
}

/// 从 JSON 文本中提取 version 字段
fn extract_version(text: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(text)
        .ok()
        .and_then(|v| v.get("version").and_then(|s| s.as_str()).map(|s| s.to_string()))
}

/// 读取本地 manifest.json 的版本号
fn get_local_version(install_path: &PathBuf) -> String {
    let manifest_path = install_path.join("manifest.json");
    if manifest_path.exists() {
        if let Ok(content) = fs::read_to_string(&manifest_path) {
            if let Some(version) = extract_version(&content) {
                return version;
            }
        }
    }
    "0.0.0".to_string()
}

/// 获取应用信息（运行时传入环境参数）
#[tauri::command]
fn get_app_info(env: String) -> UpdateInfo {
    let install_path = get_install_path(&env);
    let current_version = get_local_version(&install_path);
    UpdateInfo {
        install_path: install_path.to_string_lossy().to_string(),
        current_version,
        download_url: get_download_url(&env),
        env,
    }
}

/// 获取已保存的自定义安装路径，供前端回显当前缓存值
/// # Arguments
/// * `env` - 环境标识 "online" 或 "test"
#[tauri::command]
fn get_saved_path(env: String) -> Option<String> {
    let config_file = get_runtime_config_file();
    load_saved_path_from_file(&config_file, &env)
}

/// 保存用户自定义安装路径到持久化配置文件
/// # Arguments
/// * `env` - 环境标识
/// * `path` - 用户指定路径
#[tauri::command]
fn save_custom_path(env: String, path: String) -> Result<(), String> {
    let config_file = get_runtime_config_file();
    save_path_to_config_file(&config_file, &env, &path)
}

/// 日志服务实际绑定的端口。0 表示未启动成功
static LOG_SERVER_PORT: std::sync::atomic::AtomicU16 = std::sync::atomic::AtomicU16::new(0);

/// 自愈巡检间隔。比一级阈值（20 秒）小得多，保证超时后能及时发现；
/// 巡检只做内存态判定、无 IO，频繁一点也不费资源
const HEAL_INSPECT_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);

/// 全局日志写入器：客户端自身的错误（如托盘操作失败）与插件转发日志
/// 共用同一套落盘文件，而非仅打到 stdout/stderr——打包后 stderr 无人接收，
/// 之前的 eprintln! 在生产环境等于没记录
static LOG_SINK: std::sync::OnceLock<std::sync::Arc<dyn log_server::LogSink>> =
    std::sync::OnceLock::new();

/// 心跳状态表的全局句柄，供巡检看板相关命令读取（DEV-125034）。
/// 与 HTTP 服务、自愈巡检共享同一实例——三者必须看到同一份状态
static HEARTBEATS: std::sync::OnceLock<
    std::sync::Arc<std::sync::Mutex<heartbeat::HeartbeatRegistry>>,
> = std::sync::OnceLock::new();

/// 构建客户端自身错误的日志行（可测试的纯函数部分）
pub fn build_client_error_line(message: &str) -> String {
    format_log_line(&current_timestamp(), "error", "client", "client", message)
}

/// 记录客户端自身产生的错误。日志服务未就绪时回落到 eprintln!，
/// 保证初始化早期阶段的报错不会被静默丢弃
pub fn log_client_error(message: &str) {
    let line = build_client_error_line(message);
    match LOG_SINK.get() {
        Some(sink) => sink.write_line(&line, true),
        None => eprintln!("{}", line),
    }
}

/// 构建客户端自身普通信息的日志行（可测试的纯函数部分）
pub fn build_client_info_line(message: &str) -> String {
    format_log_line(&current_timestamp(), "info", "client", "client", message)
}

/// 记录客户端自身的普通信息（非异常）。
/// 与 `log_client_error` 的区别是不写入异常文件，避免污染「仅异常」视图
pub fn log_client_info(message: &str) {
    let line = build_client_info_line(message);
    match LOG_SINK.get() {
        Some(sink) => sink.write_line(&line, false),
        None => println!("{}", line),
    }
}

/// 构建启动版本上报的日志消息。
///
/// 十几台采购机器的日志汇总在一起时，靠这条判断每台各自跑的是哪个版本。
/// 自更新失败为静默设计，没有这条就无法发现某台卡在旧版没升上来。
/// 前缀固定为「客户端启动」，便于按关键词筛出各机器的版本分布
pub fn build_startup_version_line(version: &str) -> String {
    format!("客户端启动，版本 {}", version)
}

/// 获取日志服务端口，供前端展示与插件侧发现（0 表示服务未启动）
#[tauri::command]
fn get_log_server_port() -> u16 {
    LOG_SERVER_PORT.load(std::sync::atomic::Ordering::Relaxed)
}

/// 记录更新器自身的自动更新失败。
/// 自更新失败对采购同事既看不懂也无从处理，故前端不弹错，改为落盘由排查者查看
#[tauri::command]
fn log_self_update_error(message: String) {
    log_client_error(&format!("自动更新失败: {}", message));
}

/// 记录自更新过程中的进展（下载完成、开始安装等）。
/// INFO 级不进异常文件——这些是排查线索而非故障：故障机上「一直显示下载中」时，
/// 靠这条才能区分是没开始下载、下到一半中断、还是下完卡在安装
#[tauri::command]
fn log_self_update_info(message: String) {
    log_client_info(&format!("自动更新: {}", message));
}

/// 获取开机自启偏好，供前端/托盘菜单回显开关状态
#[tauri::command]
fn get_autostart() -> bool {
    load_autostart_from_file(&get_runtime_config_file())
}

/// 设置开机自启：同时写入配置并调用系统级注册（注册表 / LaunchAgent）
/// 配置与系统状态需保持一致，任一失败都返回 Err 以避免开关状态与实际行为不符
#[tauri::command]
fn set_autostart(app: tauri::AppHandle, enabled: bool) -> Result<(), String> {
    use tauri_plugin_autostart::ManagerExt;
    let manager = app.autolaunch();
    if enabled {
        manager
            .enable()
            .map_err(|e| format!("开启开机自启失败: {}", e))?;
    } else {
        manager
            .disable()
            .map_err(|e| format!("关闭开机自启失败: {}", e))?;
    }
    save_autostart_to_file(&get_runtime_config_file(), enabled)
}

/// 打开日志目录（供托盘菜单与前端调用，Task 1.3 落盘后即有内容）
#[tauri::command]
fn open_log_dir() -> Result<String, String> {
    let dir = get_log_dir();
    fs::create_dir_all(&dir).map_err(|e| format!("创建日志目录失败: {}", e))?;
    #[cfg(target_os = "macos")]
    let program = "open";
    #[cfg(target_os = "windows")]
    let program = "explorer";
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let program = "xdg-open";

    std::process::Command::new(program)
        .arg(&dir)
        .spawn()
        .map_err(|e| format!("打开日志目录失败: {}", e))?;
    Ok(dir.to_string_lossy().to_string())
}

/// 列出当前有日志的日期（去重、按最新优先排序），供前端日志查看页日期下拉框使用
#[tauri::command]
fn list_log_dates() -> Vec<String> {
    log_file::list_log_dates_in_dir(&get_log_dir())
}

/// read_log_entries 的返回结构：日志条目 + 当天出现过的插件名去重列表，
/// 供前端筛选下拉框展示可选项，避免为此再单独调用一次命令
#[derive(Debug, Serialize)]
struct LogEntriesResult {
    entries: Vec<log_file::LogEntry>,
    plugin_names: Vec<String>,
}

/// 读取指定日期的日志条目
/// # Arguments
/// * `date` - 格式 YYYY-MM-DD
/// * `error_only` - true 时只读异常（ERROR/WARN）日志文件
#[tauri::command]
fn read_log_entries(date: String, error_only: bool) -> Result<LogEntriesResult, String> {
    let entries = log_file::read_log_entries_from_dir(&get_log_dir(), &date, error_only)?;
    let plugin_names = log_file::collect_plugin_names(&entries);
    Ok(LogEntriesResult {
        entries,
        plugin_names,
    })
}

/// 按筛选条件分页读取日志（日志查看页专用，DEV-122550）。
///
/// 取代前端「全量拉取 + 内存过滤 + 全量渲染」的老路子：一台机器跑 3~15 个
/// Chrome 实例，当天日志可达 GB 级，全量拉取会在读文件、IPC 序列化、DOM
/// 渲染三处各卡一次。筛选与分页都在后端做，前端只拿当前这一页。
#[tauri::command]
fn read_log_page(date: String, query: log_file::LogQuery) -> Result<log_file::LogPage, String> {
    log_file::read_log_page_from_dir(&get_log_dir(), &date, &query)
}

/// 刷新所有 Chrome 浏览器标签页
/// macOS 使用 AppleScript，Windows 使用 PowerShell
/// Chrome 未运行时不报错，直接返回跳过消息
#[tauri::command]
fn refresh_chrome_tabs() -> Result<String, String> {
    run_refresh_chrome_tabs_os()
}

/// 打开 AIChat 插件侧边栏（模拟按下插件注册的 Ctrl+Shift+L 快捷键）。
///
/// # ⚠️ 会抢占全局焦点，仅供人工按需触发
/// 实现是「AppActivate 抢焦点 + SendKeys 发按键」，而焦点是**全局唯一**资源：
/// 一台机器跑着 3~15 个 Chrome 实例，插件正通过它们往供应商聊天框输入文字。
/// 抢焦点会打断其中正在输入的那个实例（不限于目标实例），按键可能落进聊天
/// 输入框造成乱字符或丢字——那是发给供应商的内容被污染。
///
/// 故**不得**由自愈流程或任何定时任务自动调用（2026-08-21 已从巡检里摘除）。
/// 保留此命令只为人工排查：使用者知道自己在干什么、且能确认此刻无人在输入。
#[tauri::command]
fn open_plugin_sidepanel() -> Result<String, String> {
    run_open_sidepanel_os()
}

// ─────────────────────────────────────────────────────────────────
// 巡检看板（DEV-125034）
//
// 替代人工日常巡检：开发原本每天要逐个点开 15 个浏览器，逐一查看 WS 是否
// 连接、1688 是否登出、窗口是否被最小化。这些判断插件内部本来就在做，只是
// 各自展示在自己的 sidepanel/badge 里；客户端本就在收全部实例的心跳，
// 汇总本该由它做。
// ─────────────────────────────────────────────────────────────────

/// 构建 Windows PowerShell 命令：列出所有 Chrome 窗口及其是否最小化。
///
/// 输出每行 `<进程id>|<0或1>`，1 表示最小化。用 WindowStyle 判断而非
/// IsIconic：前者 PowerShell 直接可读，不必 P/Invoke user32
pub fn build_list_chrome_windows_command_windows() -> String {
    "Get-Process -Name chrome -ErrorAction SilentlyContinue | \
     Where-Object { $_.MainWindowHandle -ne 0 } | \
     ForEach-Object { \
       $min = if ($_.MainWindowTitle -eq '') { 1 } else { 0 }; \
       Write-Output \"$($_.Id)|$min\" \
     }"
    .to_string()
}

/// 解析上面命令的输出，返回 (进程id, 是否最小化) 列表。
///
/// 容忍空行与格式异常行（跳过而非整体失败）——巡检看板缺一行数据
/// 远好过整个页面报错
pub fn parse_chrome_window_states(output: &str) -> Vec<(u32, bool)> {
    output
        .lines()
        .filter_map(|line| {
            let (pid, min) = line.trim().split_once('|')?;
            Some((pid.trim().parse::<u32>().ok()?, min.trim() == "1"))
        })
        .collect()
}

/// 巡检看板数据：实例状态 + 窗口概况。
/// 字段统一驼峰，与 PluginSnapshot 保持一致，免得前端两种风格混用
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct PatrolReport {
    /// 各实例快照，异常的已排在前面
    instances: Vec<heartbeat::PluginSnapshot>,
    /// Chrome 窗口总数（客户端直接枚举得到，插件侧拿不到）
    chrome_windows: usize,
    /// 其中已最小化的窗口数
    minimized_windows: usize,
}

/// 读取巡检看板数据
#[tauri::command]
fn get_patrol_report() -> Result<PatrolReport, String> {
    let instances = match HEARTBEATS.get() {
        Some(reg) => match reg.lock() {
            Ok(r) => r.snapshots(std::time::Instant::now()),
            // 锁被毒化时返回空列表而非报错：看板打不开比少一次数据更糟
            Err(_) => Vec::new(),
        },
        None => Vec::new(),
    };
    let windows = list_chrome_windows_os().unwrap_or_default();
    Ok(PatrolReport {
        chrome_windows: windows.len(),
        minimized_windows: windows.iter().filter(|(_, min)| *min).count(),
        instances,
    })
}

/// 给指定实例下发一条指令（当前用于重连 WS）。
///
/// 走「路径 A」——指令通过心跳响应捎给发心跳的那个实例，天然定向、
/// 不抢焦点、无手势要求。这是当前唯一能真正闭环的自愈手段：侧边栏仍
/// 开着，重连后立刻可继续干活（不像 reload 会连带打掉侧边栏）
#[tauri::command]
fn send_plugin_command(plugin_name: String, kind: String) -> Result<String, String> {
    // 只允许已知的安全指令，避免前端传入任意字符串
    const ALLOWED: [&str; 1] = ["reconnectWs"];
    if !ALLOWED.contains(&kind.as_str()) {
        return Err(format!("不支持的指令类型: {}", kind));
    }
    let reg = HEARTBEATS.get().ok_or("心跳服务未启动")?;
    let mut guard = reg.lock().map_err(|_| "心跳状态表不可用".to_string())?;
    match guard.enqueue_command(&plugin_name, &kind) {
        Some(_) => {
            log_client_info(&format!("[巡检] 已向 {} 下发 {} 指令", plugin_name, kind));
            Ok(format!("已下发，等待 {} 执行", plugin_name))
        }
        None => Ok("该指令已在队列中，或实例未上报过心跳".to_string()),
    }
}

/// 枚举 Chrome 窗口及最小化状态
#[cfg(target_os = "windows")]
fn list_chrome_windows_os() -> Result<Vec<(u32, bool)>, String> {
    let cmd = build_list_chrome_windows_command_windows();
    let output = std::process::Command::new("powershell")
        .args(["-Command", &cmd])
        .output()
        .map_err(|e| format!("枚举 Chrome 窗口失败: {}", e))?;
    Ok(parse_chrome_window_states(
        &String::from_utf8_lossy(&output.stdout),
    ))
}

/// macOS 下用 AppleScript 数 Chrome 窗口。仅供本机开发调试——
/// 采购机器全部是 Windows
#[cfg(target_os = "macos")]
fn list_chrome_windows_os() -> Result<Vec<(u32, bool)>, String> {
    let script = "tell application \"Google Chrome\" to get count of windows";
    let output = std::process::Command::new("osascript")
        .args(["-e", script])
        .output()
        .map_err(|e| format!("枚举 Chrome 窗口失败: {}", e))?;
    let n: usize = String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .unwrap_or(0);
    // macOS 分支不区分最小化，仅返回窗口数量占位
    Ok((0..n).map(|i| (i as u32, false)).collect())
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn list_chrome_windows_os() -> Result<Vec<(u32, bool)>, String> {
    Ok(Vec::new())
}

/// 启动自愈巡检后台线程。
///
/// 判定放在客户端本地而非中心：网断了、中心挂了，本机自愈照常工作；
/// 且判定在本地是秒级反应，绕中心要等好几个网络往返。
///
/// 本函数只负责「把判定结果落成实际动作」，判定逻辑全在 heartbeat 模块（可单测）
fn spawn_heal_inspector(registry: std::sync::Arc<std::sync::Mutex<heartbeat::HeartbeatRegistry>>) {
    std::thread::spawn(move || loop {
        std::thread::sleep(HEAL_INSPECT_INTERVAL);
        let actions = match registry.lock() {
            Ok(mut reg) => reg.inspect(std::time::Instant::now()),
            // 锁被毒化时跳过本轮而非让巡检线程整体退出——
            // 退出等于自愈永久失效，比跳过一轮严重得多
            Err(_) => continue,
        };
        for (plugin, action) in actions {
            match action {
                heartbeat::HealAction::None => {}
                heartbeat::HealAction::IssueReload => {
                    // 指令已入队，等插件下次心跳自取；这里只记录便于排查
                    log_client_info(&format!("[自愈] {} 心跳超时，已下发 reload 指令", plugin));
                }
                // 侧边栏未打开：只记录，**不自动拉起**（2026-08-21 止血）。
                // 拉起只能靠抢全局焦点 + 模拟按键，而一台机器上跑着 3~15 个
                // Chrome 实例、插件正在往供应商聊天框输入文字——抢焦点会打断
                // 其中正在输入的那个，按键落进输入框会污染发出去的聊天内容。
                // 焦点是全局唯一资源，加节流也只是降低频率、不改变性质。
                heartbeat::HealAction::SidepanelClosed => {
                    log_client_error(&format!(
                        "[自愈] {} 侧边栏未打开（自动拉起已停用，避免抢焦点干扰插件输入），需人工处理",
                        plugin
                    ));
                }
                // 已上报过，不重复记录——巡检每 5 秒一轮
                heartbeat::HealAction::SidepanelClosedSilently => {}
                // 二级自愈（重启 Chrome）暂不启用：一台机器会多开 2~3 个 Chrome
                // 实例、各自登录不同 CJ 账号，重启会把全部实例一起干掉，正在跑的
                // 采购任务全断——代价远大于收益。判定逻辑与执行命令均已实现并测试
                // 通过（heartbeat 模块 + run_restart_chrome_os），确认收益大于风险
                // 后接线即可，此处只记录以便观察真实发生频率
                heartbeat::HealAction::RestartChrome => {
                    log_client_error(&format!(
                        "[自愈] {} 彻底失联（已达重启条件，但重启 Chrome 暂未启用），等待人工介入",
                        plugin
                    ));
                }
                heartbeat::HealAction::RestartSuppressed(reason) => {
                    log_client_error(&format!(
                        "[自愈] {} 彻底失联（{}），等待人工介入",
                        plugin, reason
                    ));
                }
                // 同一原因已上报过，不重复记录——巡检每 5 秒一轮，
                // 不去重会让一台故障机一天攒出上千条相同日志
                heartbeat::HealAction::RestartSuppressedSilently => {}
            }
        }
    });
}

/// 重启 Chrome 并把插件侧边栏拉起来（二级自愈的完整动作）。
///
/// 两步必须连在一起：只重启浏览器的话，插件虽然重新加载了，但
/// sidepanel 不会自动打开——而插件设计上要求 sidepanel 常驻才能处理任务，
/// 等于自愈只做了一半。等待时间给 Chrome 启动与插件初始化留余量
#[tauri::command]
fn restart_chrome_and_open_sidepanel() -> Result<String, String> {
    let restart = run_restart_chrome_os()?;
    // Chrome 冷启动 + 插件 Service Worker 初始化需要时间，
    // 过早发快捷键会因插件尚未注册 command 监听而丢失
    std::thread::sleep(std::time::Duration::from_secs(5));
    let sidepanel = run_open_sidepanel_os()?;
    Ok(format!("{}；{}", restart, sidepanel))
}

/// 检查更新（对比本地与远程版本）
#[tauri::command]
async fn check_update(env: String) -> Result<CheckResult, String> {
    let install_path = get_install_path(&env);
    let current_version = get_local_version(&install_path);
    let download_url = get_download_url(&env);
    let manifest_url = download_url.replace("aichat.zip", "manifest.json");

    let client = build_http_client(&env).map_err(|e| format!("创建HTTP客户端失败: {}", e))?;

    let remote_version = match client.get(&manifest_url).send().await {
        Ok(resp) => {
            if resp.status().is_success() {
                match resp.text().await {
                    Ok(text) => {
                        extract_version(&text)
                            .ok_or_else(|| format!("manifest.json 中未找到 version 字段，响应内容片段: {}", &text[..text.len().min(200)]))?                    }
                    Err(e) => return Err(format!("读取响应内容失败: {}", e)),
                }
            } else {
                return Err(format!("获取 manifest 失败，HTTP状态码: {}", resp.status()));
            }
        }
        Err(e) => return Err(format!("网络请求失败: {}", e)),
    };

    let has_update = version_compare(&remote_version, &current_version);

    Ok(CheckResult {
        has_update,
        current_version,
        remote_version,
        install_path: install_path.to_string_lossy().to_string(),
    })
}

/// 版本号比较：remote > local 返回 true
fn version_compare(remote: &str, local: &str) -> bool {
    let parse = |v: &str| -> Vec<u64> {
        v.split('.')
            .filter_map(|s| s.parse::<u64>().ok())
            .collect()
    };
    let r = parse(remote);
    let l = parse(local);
    for i in 0..r.len().max(l.len()) {
        let rv = r.get(i).copied().unwrap_or(0);
        let lv = l.get(i).copied().unwrap_or(0);
        if rv > lv {
            return true;
        }
        if rv < lv {
            return false;
        }
    }
    false
}

/// 执行更新：下载 ZIP 并解压到安装路径
#[tauri::command]
async fn perform_update(env: String) -> Result<String, String> {
    let install_path = get_install_path(&env);
    let download_url = get_download_url(&env);

    fs::create_dir_all(&install_path)
        .map_err(|e| format!("创建目录失败: {}", e))?;

    let temp_dir = tempfile::tempdir()
        .map_err(|e| format!("创建临时目录失败: {}", e))?;
    let zip_path = temp_dir.path().join("aichat.zip");

    let client = build_http_client(&env).map_err(|e| format!("创建HTTP客户端失败: {}", e))?;

    let response = client
        .get(&download_url)
        .send()
        .await
        .map_err(|e| format!("下载失败: {}", e))?;

    if !response.status().is_success() {
        return Err(format!("下载失败，HTTP状态码: {}", response.status()));
    }

    let bytes = response
        .bytes()
        .await
        .map_err(|e| format!("读取下载内容失败: {}", e))?;

    fs::write(&zip_path, &bytes)
        .map_err(|e| format!("保存ZIP文件失败: {}", e))?;

    let file = fs::File::open(&zip_path)
        .map_err(|e| format!("打开ZIP文件失败: {}", e))?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|e| format!("解析ZIP文件失败: {}", e))?;

    for i in 0..archive.len() {
        let mut file = archive
            .by_index(i)
            .map_err(|e| format!("读取ZIP条目失败: {}", e))?;

        let file_name = file.name().to_string();
        if file_name.contains("..") {
            continue;
        }

        let outpath = install_path.join(&file_name);

        if file.is_dir() {
            fs::create_dir_all(&outpath)
                .map_err(|e| format!("创建目录失败: {}", e))?;
        } else {
            if let Some(parent) = outpath.parent() {
                fs::create_dir_all(parent)
                    .map_err(|e| format!("创建父目录失败: {}", e))?;
            }
            let mut outfile = fs::File::create(&outpath)
                .map_err(|e| format!("创建文件失败: {}", e))?;

            let mut buffer = Vec::new();
            file.read_to_end(&mut buffer)
                .map_err(|e| format!("读取文件失败: {}", e))?;
            outfile
                .write_all(&buffer)
                .map_err(|e| format!("写入文件失败: {}", e))?;
        }
    }

    let new_version = get_local_version(&install_path);
    Ok(format!("更新完成！当前版本: {}", new_version))
}

// ─────────────────────────────────────────────────────────────────
// 机器状态监测（DEV-124169）：采购同事在虚拟机上使用，怀疑资源紧张
// 导致插件卡顿或更新不稳定，靠这组信息辅助判断——本版只在客户端
// 本地展示，不做远程上报（现有日志体系本身也全是本地落盘）
// ─────────────────────────────────────────────────────────────────

/// 单块磁盘的简化信息（脱离 sysinfo::Disks 的具体类型，便于构造测试数据）
struct DiskInfo {
    mount_point: String,
    total_bytes: u64,
    available_bytes: u64,
}

/// 一次性采集到的原始硬件数据（脱离 sysinfo::System 的具体类型，便于构造测试数据）
struct HardwareSample {
    total_memory_bytes: u64,
    available_memory_bytes: u64,
    cpu_brand: String,
    cpu_cores: usize,
    cpu_usage_percent: f32,
    os_version: String,
}

/// 前端展示用的机器状态快照
#[derive(Debug, Serialize, Deserialize)]
struct SystemSnapshot {
    total_memory_bytes: u64,
    available_memory_bytes: u64,
    cpu_brand: String,
    cpu_cores: usize,
    cpu_usage_percent: f32,
    disk_total_bytes: u64,
    disk_available_bytes: u64,
    os_version: String,
}

/// 在磁盘列表中选出安装路径所在的那一块。
/// 按挂载点字符串做最长前缀匹配（Windows 盘符如 "D:\\" 同样适用，
/// 一个前缀刚好对应一块盘，不会有更长的候选）；找不到匹配项时
/// 回退到列表第一项，保守给出一个可用磁盘而非空数据
fn pick_disk_for_path<'a>(disks: &'a [DiskInfo], install_path: &Path) -> Option<&'a DiskInfo> {
    let path_str = install_path.to_string_lossy();
    disks
        .iter()
        .filter(|d| path_str.starts_with(d.mount_point.as_str()))
        .max_by_key(|d| d.mount_point.len())
        .or_else(|| disks.first())
}

/// 把采集到的原始数据 + 磁盘列表 + 安装路径，组装成前端展示用的快照（可测试的纯函数）
fn build_system_snapshot(
    hw: HardwareSample,
    disks: &[DiskInfo],
    install_path: &Path,
) -> SystemSnapshot {
    let disk = pick_disk_for_path(disks, install_path);
    SystemSnapshot {
        total_memory_bytes: hw.total_memory_bytes,
        available_memory_bytes: hw.available_memory_bytes,
        cpu_brand: hw.cpu_brand,
        cpu_cores: hw.cpu_cores,
        cpu_usage_percent: hw.cpu_usage_percent,
        disk_total_bytes: disk.map(|d| d.total_bytes).unwrap_or(0),
        disk_available_bytes: disk.map(|d| d.available_bytes).unwrap_or(0),
        os_version: hw.os_version,
    }
}

/// 采集当前机器的 CPU/内存/磁盘/系统版本信息，供前端「机器状态」页展示。
/// CPU 占用率需要两次采样取差值才准确（sysinfo 官方建议），故刷新两次、
/// 间隔略大于 sysinfo 内部最小采样间隔
#[tauri::command]
async fn get_system_snapshot(env: String) -> SystemSnapshot {
    use sysinfo::{Disks, System};

    let mut sys = System::new_all();
    sys.refresh_cpu_usage();
    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    sys.refresh_cpu_usage();

    let cpu_usage_percent = if sys.cpus().is_empty() {
        0.0
    } else {
        sys.cpus().iter().map(|c| c.cpu_usage()).sum::<f32>() / sys.cpus().len() as f32
    };
    let cpu_brand = sys
        .cpus()
        .first()
        .map(|c| c.brand().to_string())
        .unwrap_or_else(|| "未知".to_string());

    let hw = HardwareSample {
        total_memory_bytes: sys.total_memory(),
        available_memory_bytes: sys.available_memory(),
        cpu_brand,
        cpu_cores: sys.cpus().len(),
        cpu_usage_percent,
        os_version: System::long_os_version().unwrap_or_else(|| "未知".to_string()),
    };

    let disks = Disks::new_with_refreshed_list();
    let disk_infos: Vec<DiskInfo> = disks
        .iter()
        .map(|d| DiskInfo {
            mount_point: d.mount_point().to_string_lossy().to_string(),
            total_bytes: d.total_space(),
            available_bytes: d.available_space(),
        })
        .collect();

    let install_path = get_install_path(&env);
    build_system_snapshot(hw, &disk_infos, &install_path)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        // 单实例锁：常驻后重复启动只唤回已有窗口，避免多进程抢占日志端口（Task 1.2）
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            show_main_window(app);
        }))
        // 开机自启：默认不注册，由用户在托盘菜单主动开启
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        // 桌面端自更新：十几台采购虚拟机不可能逐台手动替换程序文件，
        // 由常驻进程定时拉取 latest.json 静默升级自身
        .plugin(tauri_plugin_updater::Builder::new().build())
        // 供更新安装完成后重启应用
        .plugin(tauri_plugin_process::init())
        .setup(|app| {
            build_tray(app.handle())?;
            // 版本号取自 tauri.conf.json（package_info 的来源），与 updater 比对所用版本同源。
            // 不用 CARGO_PKG_VERSION —— 那是 Cargo.toml 的版本，两者不同步时会出现
            // 「实际跑新版、对外自报旧版」，排查时被自己的日志误导
            let app_version = app.package_info().version.to_string();
            // 日志服务启动失败不阻断应用：更新功能仍应可用，仅退化为不收集日志
            let sink: std::sync::Arc<dyn log_server::LogSink> =
                std::sync::Arc::new(log_file::FileSink::new(get_log_dir()));
            let _ = LOG_SINK.set(sink.clone());
            // 启动即记录自身版本：自更新失败是静默的（采购同事看到报错也无从处理），
            // 没有这条日志就无法判断十几台机器里谁卡在旧版没升上来
            log_client_info(&build_startup_version_line(&app_version));
            // 心跳状态表由 HTTP 服务与自愈巡检共享同一实例：
            // 巡检要读 HTTP 侧写入的 last_seen，也要往队列塞待下发指令
            let heartbeats = std::sync::Arc::new(std::sync::Mutex::new(
                heartbeat::HeartbeatRegistry::new(),
            ));
            // 供巡检看板命令读取同一份状态
            let _ = HEARTBEATS.set(heartbeats.clone());
            match log_server::spawn(sink, &app_version, heartbeats.clone()) {
                Ok(port) => {
                    LOG_SERVER_PORT.store(port, std::sync::atomic::Ordering::Relaxed);
                    println!(
                        "日志服务已启动: http://127.0.0.1:{}，日志目录: {:?}",
                        port,
                        get_log_dir()
                    );
                    // 仅在日志服务起来后才启动巡检：服务没起来时插件根本发不了
                    // 心跳，巡检只会把「从未上报」误判成失联而反复重启 Chrome
                    spawn_heal_inspector(heartbeats);
                }
                Err(e) => log_client_error(&format!("日志服务启动失败（不影响更新功能）: {}", e)),
            }
            Ok(())
        })
        // 关窗口不退进程：常驻看护的前提，改为隐藏到托盘
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_app_info,
            check_update,
            perform_update,
            get_saved_path,
            save_custom_path,
            refresh_chrome_tabs,
            get_autostart,
            set_autostart,
            open_log_dir,
            get_log_server_port,
            log_self_update_error,
            log_self_update_info,
            list_log_dates,
            read_log_entries,
            read_log_page,
            get_system_snapshot,
            open_plugin_sidepanel,
            restart_chrome_and_open_sidepanel,
            get_patrol_report,
            send_plugin_command,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

/// 显示并聚焦主窗口（托盘点击、单实例唤回共用）
fn show_main_window(app: &tauri::AppHandle) {
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.show();
        let _ = win.unminimize();
        let _ = win.set_focus();
    }
}

/// 构建系统托盘：图标常驻 + 菜单（显示窗口 / 打开日志目录 / 开机自启 / 退出）
fn build_tray(app: &tauri::AppHandle) -> tauri::Result<()> {
    use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
    use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};

    let show_item = MenuItem::with_id(app, "show", "显示主窗口", true, None::<&str>)?;
    let log_item = MenuItem::with_id(app, "open_log", "打开日志目录", true, None::<&str>)?;
    // 不必等 4 小时的轮询：发版后想立刻让某台机器升级时用
    let check_item = MenuItem::with_id(app, "check_update", "立即检查更新", true, None::<&str>)?;
    let autostart_item = CheckMenuItem::with_id(
        app,
        "toggle_autostart",
        "开机自启",
        true,
        load_autostart_from_file(&get_runtime_config_file()),
        None::<&str>,
    )?;
    let quit_item = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[
            &show_item,
            &log_item,
            &check_item,
            &PredefinedMenuItem::separator(app)?,
            &autostart_item,
            &PredefinedMenuItem::separator(app)?,
            &quit_item,
        ],
    )?;

    TrayIconBuilder::with_id("main-tray")
        .icon(app.default_window_icon().unwrap().clone())
        .tooltip("aichat 插件更新工具")
        .menu(&menu)
        // 左键单击托盘图标唤回窗口（Windows 习惯用法；macOS 由菜单承担）
        .show_menu_on_left_click(false)
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main_window(tray.app_handle());
            }
        })
        .on_menu_event(move |app, event| match event.id().as_ref() {
            "show" => show_main_window(app),
            // 检查逻辑在前端（updater 插件的 JS API），故这里只发事件通知；
            // 同时唤回窗口，否则用户看不到「已是最新版本」之类的反馈
            "check_update" => {
                show_main_window(app);
                let _ = app.emit("tray://check-update", ());
            }
            "open_log" => {
                if let Err(e) = open_log_dir() {
                    log_client_error(&format!("打开日志目录失败: {}", e));
                }
            }
            "toggle_autostart" => {
                let config_file = get_runtime_config_file();
                let next = !load_autostart_from_file(&config_file);
                match set_autostart(app.clone(), next) {
                    // 系统注册成功后才同步勾选态，失败则保持原状避免显示与实际不符
                    Ok(()) => {
                        if let Some(item) = app.menu().and_then(|m| m.get("toggle_autostart")) {
                            if let Some(check) = item.as_check_menuitem() {
                                let _ = check.set_checked(next);
                            }
                        }
                    }
                    Err(e) => log_client_error(&format!("切换开机自启失败: {}", e)),
                }
            }
            "quit" => app.exit(0),
            _ => {}
        })
        .build(app)?;
    Ok(())
}

// ─────────────────────────────────────────────────────────────────
// 测试模块（TDD 先写用例，再实现）
// ─────────────────────────────────────────────────────────────────
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    // 1. 配置文件路径
    #[test]
    fn test_config_file_path_is_in_given_dir() {
        let dir = PathBuf::from("/tmp/aichat-test-config");
        let path = get_config_file_path_with_dir(&dir);
        assert_eq!(path, dir.join("config.json"), "配置文件路径应为 <dir>/config.json");
    }

    // 2. 路径写入与读取
    #[test]
    fn test_save_and_load_custom_path() {
        let tmp = TempDir::new().expect("创建临时目录失败，会导致路径持久化测试无法运行");
        let config_file = tmp.path().join("config.json");
        let custom_path = "/custom/path/aichat";

        save_path_to_config_file(&config_file, "online", custom_path)
            .expect("保存自定义路径失败，会导致用户路径设定无法持久化");

        let loaded = load_saved_path_from_file(&config_file, "online");
        assert_eq!(
            loaded,
            Some(custom_path.to_string()),
            "读取的路径与保存的路径不一致，会导致用户重启后路径丢失"
        );
    }

    #[test]
    fn test_load_returns_none_when_config_missing() {
        let config_file = PathBuf::from("/tmp/non_existent_path_12345/config.json");
        let result = load_saved_path_from_file(&config_file, "online");
        assert!(
            result.is_none(),
            "配置文件不存在时应返回 None，避免崩溃影响正常使用"
        );
    }

    // 3. 多环境隔离
    #[test]
    fn test_online_and_test_env_paths_are_isolated() {
        let tmp = TempDir::new().expect("创建临时目录失败");
        let config_file = tmp.path().join("config.json");

        save_path_to_config_file(&config_file, "online", "/custom/online_path")
            .expect("保存 online 路径失败");
        save_path_to_config_file(&config_file, "test", "/custom/test_path")
            .expect("保存 test 路径失败");

        let online = load_saved_path_from_file(&config_file, "online");
        let test = load_saved_path_from_file(&config_file, "test");

        assert_eq!(
            online,
            Some("/custom/online_path".to_string()),
            "online 路径与 test 路径混淆，会导致误装到错误目录"
        );
        assert_eq!(
            test,
            Some("/custom/test_path".to_string()),
            "test 路径与 online 路径混淆，会导致误装到错误目录"
        );
    }

    #[test]
    fn test_overwrite_custom_path() {
        let tmp = TempDir::new().expect("创建临时目录失败");
        let config_file = tmp.path().join("config.json");

        save_path_to_config_file(&config_file, "online", "/old/path").expect("第一次保存失败");
        save_path_to_config_file(&config_file, "online", "/new/path").expect("第二次保存失败");

        let result = load_saved_path_from_file(&config_file, "online");
        assert_eq!(
            result,
            Some("/new/path".to_string()),
            "覆盖写入失败，路径未更新，会导致用户修改后仍使用旧路径"
        );
    }

    // 4. 安装路径解析（优先自定义 > 默认）
    #[test]
    fn test_install_path_uses_custom_when_provided() {
        let custom = Some("/custom/install/aichat".to_string());
        let path = get_install_path_resolved("online", custom);
        assert_eq!(
            path,
            PathBuf::from("/custom/install/aichat"),
            "提供自定义路径时应使用自定义路径，否则无法解决 C 盘限制问题"
        );
    }

    #[test]
    fn test_install_path_falls_back_to_default() {
        #[cfg(not(target_os = "windows"))]
        {
            let path = get_install_path_resolved("online", None);
            let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/tmp"));
            let expected = home.join("aichat");
            assert_eq!(
                path, expected,
                "无自定义路径时应回退到默认路径 ~/aichat，否则无法正常安装"
            );
        }
    }

    #[test]
    fn test_install_path_test_env_has_test_suffix() {
        #[cfg(not(target_os = "windows"))]
        {
            let path = get_install_path_resolved("test", None);
            let path_str = path.to_string_lossy();
            assert!(
                path_str.ends_with("aichat_test"),
                "test 环境默认路径应以 aichat_test 结尾，当前为: {}",
                path_str
            );
        }
    }

    // 5. 配置文件 JSON 格式校验
    #[test]
    fn test_saved_config_is_valid_json() {
        let tmp = TempDir::new().expect("创建临时目录失败");
        let config_file = tmp.path().join("config.json");
        save_path_to_config_file(&config_file, "online", "/some/path").expect("保存路径失败");
        let content = std::fs::read_to_string(&config_file).expect("读取配置文件失败");
        let parsed: serde_json::Result<serde_json::Value> = serde_json::from_str(&content);
        assert!(
            parsed.is_ok(),
            "配置文件不是合法 JSON，会导致下次读取时解析失败: {}",
            content
        );
    }

    // 6. Chrome 标签页刷新脚本构建
    #[test]
    fn test_build_macos_refresh_script_structure() {
        let script = build_refresh_all_tabs_script_macos();
        assert!(
            script.contains("Google Chrome"),
            "macOS 刷新脚本应包含 Google Chrome 应用名"
        );
        assert!(
            script.contains("reload t"),
            "macOS 刷新脚本应包含 reload 命令"
        );
        assert!(
            script.contains("repeat with t in tabs"),
            "macOS 刷新脚本应遍历所有标签页，否则只会刷新单个标签"
        );
        assert!(
            script.contains("repeat with w in windows"),
            "macOS 刷新脚本应遍历所有窗口，否则多窗口场景下部分标签不会刷新"
        );
    }

    #[test]
    fn test_build_windows_refresh_command_structure() {
        let cmd = build_refresh_all_tabs_command_windows();
        assert!(
            cmd.contains("chrome"),
            "Windows 刷新命令应包含 chrome 进程名"
        );
        assert!(
            cmd.contains("Refresh"),
            "Windows 刷新命令应包含 Refresh 方法调用"
        );
    }

    // ─────────────────────────────────────────────────────────
    // 自动打开插件侧边栏（DEV-124702 阶段一）
    //
    // Chrome 要求 sidePanel.open() 必须由用户手势触发，外部进程无法直接
    // 调用该 API（2026-07-30 的 af1233b 已验证「拼 chrome-extension:// URL
    // 导航」这条路走不通）。改为模拟按下插件已注册的快捷键 Ctrl+Shift+L
    // ——键盘事件是合法的用户手势，Chrome 会正常触发插件的 commands 监听
    // ─────────────────────────────────────────────────────────

    #[test]
    fn test_build_windows_open_sidepanel_sends_ctrl_shift_l() {
        let cmd = build_open_sidepanel_command_windows();
        assert!(
            cmd.contains("^+l"),
            "必须发送 Ctrl+Shift+L（SendKeys 记法 ^=Ctrl +=Shift）——\
             这是插件注册的 toggle_sidepanel 快捷键，换成别的键不会触发侧边栏：{}",
            cmd
        );
        assert!(
            cmd.contains("SendKeys"),
            "需要用 SendKeys 模拟真实键盘事件；直接调 Chrome API 会因缺少用户手势被拒：{}",
            cmd
        );
    }

    #[test]
    fn test_build_windows_open_sidepanel_activates_chrome_first() {
        let cmd = build_open_sidepanel_command_windows();
        assert!(
            cmd.contains("AppActivate"),
            "必须先激活 Chrome 窗口——SendKeys 是发给当前焦点窗口的，\
             不激活会把快捷键发给别的程序：{}",
            cmd
        );
        assert!(
            cmd.contains("chrome"),
            "需按进程名定位 Chrome：{}",
            cmd
        );
        // 激活窗口是异步的，紧接着发按键会丢失
        assert!(
            cmd.contains("Sleep"),
            "激活与发送之间需要等待，否则窗口尚未获得焦点、按键丢失：{}",
            cmd
        );
    }

    #[test]
    fn test_build_windows_open_sidepanel_tolerates_chrome_absent() {
        let cmd = build_open_sidepanel_command_windows();
        assert!(
            cmd.contains("SilentlyContinue") || cmd.contains("if ("),
            "Chrome 未运行时不应报错——虚拟机上 Chrome 可能还没启动，\
             此时应静默跳过而非把自愈流程整体拖挂：{}",
            cmd
        );
    }

    #[test]
    fn test_build_macos_open_sidepanel_sends_cmd_shift_l() {
        let script = build_open_sidepanel_script_macos();
        assert!(
            script.contains("command down") && script.contains("shift down"),
            "macOS 上快捷键是 Command+Shift+L（插件 suggested_key 的 mac 配置）：{}",
            script
        );
        assert!(
            script.contains("keystroke \"l\""),
            "应发送字母 l：{}",
            script
        );
        assert!(
            script.contains("activate"),
            "需先激活 Chrome，否则按键发给当前焦点程序：{}",
            script
        );
        assert!(
            script.contains("System Events"),
            "macOS 模拟按键须经 System Events：{}",
            script
        );
    }

    // ─────────────────────────────────────────────────────────
    // 重启 Chrome（DEV-124702 二级自愈）
    //
    // 插件「全死」（Service Worker 崩溃到唤不醒）时收不到 reload 指令，
    // 只能重启浏览器让插件随之重新加载。这是代价最大的手段，故：
    // 1. 优先温和退出而非强杀——强杀会被 Chrome 判定为异常崩溃，
    //    下次启动弹「恢复页面」提示，且可能丢失会话
    // 2. 重启后必须把侧边栏拉起来（插件设计上要求 sidepanel 常驻）
    // ─────────────────────────────────────────────────────────

    #[test]
    fn test_build_windows_restart_chrome_quits_gracefully_first() {
        let cmd = build_restart_chrome_command_windows();
        assert!(
            cmd.contains("CloseMainWindow"),
            "应先尝试温和关闭主窗口——直接 taskkill /F 会被 Chrome 判定为崩溃，\
             下次启动弹恢复提示，采购同事的标签页也可能丢：{}",
            cmd
        );
        assert!(
            cmd.contains("chrome"),
            "需按进程名定位 Chrome：{}",
            cmd
        );
    }

    #[test]
    fn test_build_windows_restart_chrome_has_force_kill_fallback() {
        let cmd = build_restart_chrome_command_windows();
        // 温和关闭可能因页面弹「确定要离开吗」而卡住，必须有兜底
        assert!(
            cmd.contains("Kill") || cmd.contains("taskkill"),
            "温和关闭失败时需强制结束，否则自愈会卡在这一步永远不往下走：{}",
            cmd
        );
        assert!(
            cmd.contains("Start-Process") || cmd.contains("start "),
            "关闭后必须重新拉起 Chrome，否则插件永远起不来：{}",
            cmd
        );
    }

    #[test]
    fn test_build_windows_restart_chrome_waits_between_steps() {
        let cmd = build_restart_chrome_command_windows();
        assert!(
            cmd.contains("Sleep"),
            "关闭与重启之间需等待进程真正退出，否则新实例会复用旧进程、\
             插件不会重新加载，自愈等于没做：{}",
            cmd
        );
    }

    #[test]
    fn test_build_macos_restart_chrome_structure() {
        let script = build_restart_chrome_script_macos();
        assert!(
            script.contains("quit"),
            "macOS 下用 quit 温和退出（AppleScript 无强杀语义）：{}",
            script
        );
        assert!(
            script.contains("Google Chrome"),
            "需指明目标应用：{}",
            script
        );
    }

    // ─────────────────────────────────────────────────────────
    // Task 1.1 常驻化：开机自启偏好持久化
    // ─────────────────────────────────────────────────────────

    #[test]
    fn test_autostart_defaults_to_disabled_when_config_missing() {
        let config_file = PathBuf::from("/tmp/non_existent_autostart_98765/config.json");
        assert_eq!(
            load_autostart_from_file(&config_file),
            false,
            "配置缺失时开机自启必须默认关闭，否则会在用户未同意的情况下静默修改开机项"
        );
    }

    #[test]
    fn test_save_and_load_autostart_enabled() {
        let tmp = TempDir::new().expect("创建临时目录失败，会导致开机自启偏好测试无法运行");
        let config_file = tmp.path().join("config.json");

        save_autostart_to_file(&config_file, true)
            .expect("保存开机自启偏好失败，会导致用户设置重启后丢失");

        assert_eq!(
            load_autostart_from_file(&config_file),
            true,
            "读取的自启偏好与保存值不一致，会导致托盘开关状态与实际行为不符"
        );
    }

    #[test]
    fn test_save_autostart_can_be_toggled_off() {
        let tmp = TempDir::new().expect("创建临时目录失败，会导致开机自启偏好测试无法运行");
        let config_file = tmp.path().join("config.json");

        save_autostart_to_file(&config_file, true).expect("首次开启自启失败");
        save_autostart_to_file(&config_file, false).expect("关闭自启失败");

        assert_eq!(
            load_autostart_from_file(&config_file),
            false,
            "关闭自启后仍读到开启状态，会导致用户无法真正关掉开机启动"
        );
    }

    #[test]
    fn test_save_autostart_preserves_existing_path() {
        let tmp = TempDir::new().expect("创建临时目录失败，会导致配置共存测试无法运行");
        let config_file = tmp.path().join("config.json");
        let custom_path = "/custom/aichat";

        save_path_to_config_file(&config_file, "online", custom_path).expect("保存路径失败");
        save_autostart_to_file(&config_file, true).expect("保存自启偏好失败");

        assert_eq!(
            load_saved_path_from_file(&config_file, "online"),
            Some(custom_path.to_string()),
            "写入自启偏好后安装路径丢失，会导致用户已配置的安装目录被清空"
        );
        assert_eq!(
            load_autostart_from_file(&config_file),
            true,
            "自启偏好未正确写入"
        );
    }

    // ─────────────────────────────────────────────────────────
    // 时间戳归一化：插件侧用 new Date().toISOString() 上报 UTC
    // （形如 2026-08-18T01:25:18.602Z），直接落盘会让日志页显示成
    // 比北京时间早 8 小时、且格式对非技术同事不可读
    // ─────────────────────────────────────────────────────────

    #[test]
    fn test_normalize_timestamp_converts_utc_iso_to_beijing() {
        // 插件用 toISOString() 上报的 UTC 串，须转成北京时间（+8）
        let got = normalize_timestamp("2026-08-18T01:25:18.602Z");
        assert_eq!(
            got, "2026-08-18 09:25:18.602",
            "UTC 01:25 应显示为北京时间 09:25；不转换会让排查者按错误时刻找现场"
        );
    }

    #[test]
    fn test_normalize_timestamp_converts_offset_iso_to_beijing() {
        // 带任意时区偏移的 ISO 串同样要归一到北京时间
        let got = normalize_timestamp("2026-08-18T01:25:18.602+00:00");
        assert_eq!(got, "2026-08-18 09:25:18.602");
    }

    #[test]
    fn test_normalize_timestamp_keeps_already_local_format() {
        // 服务端自己生成的本地格式（current_timestamp）原样保留，
        // 不能因为再次归一化而被平移 8 小时
        let local = "2026-08-18 09:25:18.602";
        assert_eq!(
            normalize_timestamp(local),
            local,
            "已是本地格式的时间戳不得再次偏移，否则每经一次处理就多加 8 小时"
        );
    }

    #[test]
    fn test_normalize_timestamp_falls_back_to_original_when_unparseable() {
        // 认不出的格式保留原值：日志内容比格式统一更重要，
        // 不能因为解析失败就丢掉这条日志的时间信息
        let weird = "不是时间";
        assert_eq!(normalize_timestamp(weird), weird);
    }

    // ─────────────────────────────────────────────────────────
    // Task 1.2 本地 HTTP 服务：日志行格式化 / 级别归一 / 配置透传
    // ─────────────────────────────────────────────────────────

    #[test]
    fn test_format_log_line_contains_all_fields() {
        let line = format_log_line(
            "2026-07-29 10:30:00.123",
            "error",
            "background",
            "robot-01",
            "插件崩溃",
        );
        assert!(
            line.contains("2026-07-29 10:30:00.123"),
            "日志行必须含时间戳，否则无法定位问题发生时刻"
        );
        assert!(
            line.contains("ERROR"),
            "日志级别应大写输出，便于 grep 过滤异常"
        );
        assert!(
            line.contains("background"),
            "日志行必须含来源，否则无法区分是 background 还是 content script 报的错"
        );
        assert!(
            line.contains("robot-01"),
            "日志行必须含插件名，否则多台机器的日志混在一起无法区分是谁报的"
        );
        assert!(line.contains("插件崩溃"), "日志行必须含原始消息内容");
    }

    #[test]
    fn test_format_log_line_field_order_is_ts_level_source_plugin_message() {
        let line = format_log_line("ts", "info", "sidepanel", "robot-01", "消息内容");
        assert_eq!(
            line,
            "[ts] [INFO] [sidepanel] [robot-01] 消息内容",
            "字段顺序变动会导致 parse_log_line 解析错位，插件名必须作为第 4 个方括号字段"
        );
    }

    #[test]
    fn test_format_log_line_is_single_line_even_with_newlines_in_message() {
        let line = format_log_line(
            "2026-07-29 10:30:00.000",
            "info",
            "sidepanel",
            "robot-01",
            "第一行\n第二行",
        );
        assert_eq!(
            line.matches('\n').count(),
            0,
            "消息内含换行时必须转义为单行，否则会破坏一行一条的格式、导致 AI 与 grep 解析错乱"
        );
        assert!(
            line.contains("第一行") && line.contains("第二行"),
            "转义换行不应丢失原始内容"
        );
    }

    #[test]
    fn test_normalize_log_level_maps_aliases() {
        assert_eq!(normalize_log_level("warn"), "WARN");
        assert_eq!(normalize_log_level("WARNING"), "WARN", "warning 应归一为 WARN，否则同类日志会分散在两种级别下");
        assert_eq!(normalize_log_level("err"), "ERROR");
        assert_eq!(normalize_log_level("error"), "ERROR");
        assert_eq!(normalize_log_level("debug"), "DEBUG");
        assert_eq!(normalize_log_level("info"), "INFO");
    }

    #[test]
    fn test_normalize_log_level_falls_back_to_info_for_unknown() {
        assert_eq!(
            normalize_log_level("verbose"),
            "INFO",
            "未知级别应回落 INFO 而非丢弃，否则插件传入非约定级别时日志会静默消失"
        );
        assert_eq!(normalize_log_level(""), "INFO", "空级别应回落 INFO");
    }

    #[test]
    fn test_is_error_level_only_true_for_error_and_warn() {
        assert!(is_error_level("ERROR"), "ERROR 应计入异常日志");
        assert!(is_error_level("WARN"), "WARN 应计入异常日志，供 AI 分析潜在问题");
        assert!(!is_error_level("INFO"), "INFO 不应写入异常日志，否则异常文件被噪声淹没");
        assert!(!is_error_level("DEBUG"), "DEBUG 不应计入异常");
    }

    #[test]
    fn test_save_config_preserves_unknown_fields() {
        let tmp = TempDir::new().expect("创建临时目录失败，会导致配置透传测试无法运行");
        let config_file = tmp.path().join("config.json");
        // 模拟其它程序写入的未知字段
        fs::write(
            &config_file,
            r#"{"online_path":"/a","third_party_field":"keep-me"}"#,
        )
        .expect("预置配置文件失败");

        save_autostart_to_file(&config_file, true).expect("保存自启偏好失败");

        let content = fs::read_to_string(&config_file).expect("读取配置失败");
        let parsed: serde_json::Value = serde_json::from_str(&content).expect("配置应为合法 JSON");
        assert_eq!(
            parsed.get("third_party_field").and_then(|v| v.as_str()),
            Some("keep-me"),
            "写入配置时抹掉了其它程序的字段，会导致外部写入的数据静默丢失"
        );
        assert_eq!(
            parsed.get("autostart").and_then(|v| v.as_bool()),
            Some(true),
            "自启偏好应正常写入"
        );
    }

    #[test]
    fn test_autostart_config_json_shape_is_valid() {
        let tmp = TempDir::new().expect("创建临时目录失败，会导致配置格式测试无法运行");
        let config_file = tmp.path().join("config.json");

        save_autostart_to_file(&config_file, true).expect("保存自启偏好失败");

        let content = fs::read_to_string(&config_file).expect("读取配置文件失败");
        let parsed: serde_json::Value =
            serde_json::from_str(&content).expect("配置文件不是合法 JSON，会导致后续读取全部失败");
        assert_eq!(
            parsed.get("autostart").and_then(|v| v.as_bool()),
            Some(true),
            "配置中应存在布尔字段 autostart，字段名变动会导致旧版本配置无法识别"
        );
    }

    // ─────────────────────────────────────────────────────────
    // 巡检看板：Chrome 窗口枚举（DEV-125034）
    // ─────────────────────────────────────────────────────────

    #[test]
    fn test_parse_chrome_window_states_reads_pid_and_minimized() {
        let out = "1234|0\n5678|1\n";
        let states = parse_chrome_window_states(out);
        assert_eq!(states, vec![(1234, false), (5678, true)]);
    }

    #[test]
    fn test_parse_chrome_window_states_skips_malformed_lines() {
        // 巡检看板缺一行数据远好过整个页面报错
        let out = "1234|0\n垃圾行\n\nabc|1\n5678|1\n";
        let states = parse_chrome_window_states(out);
        assert_eq!(
            states,
            vec![(1234, false), (5678, true)],
            "格式异常行应跳过而非导致整体失败"
        );
    }

    #[test]
    fn test_parse_chrome_window_states_empty_output() {
        assert!(
            parse_chrome_window_states("").is_empty(),
            "Chrome 未运行时应返回空列表而非报错"
        );
    }

    #[test]
    fn test_list_chrome_windows_command_targets_chrome_only() {
        let cmd = build_list_chrome_windows_command_windows();
        assert!(
            cmd.contains("-Name chrome"),
            "必须限定只枚举 chrome 进程，避免误列其它程序窗口"
        );
        assert!(
            cmd.contains("MainWindowHandle -ne 0"),
            "须过滤掉无窗口的后台进程——Chrome 一个实例有多个子进程，只有主进程有窗口"
        );
    }

    // ─────────────────────────────────────────────────────────
    // 客户端自身错误日志：打包后 stderr 无人接收，需落盘（复用 FileSink）
    // ─────────────────────────────────────────────────────────

    #[test]
    fn test_build_client_error_line_contains_client_source_and_error_level() {
        let line = build_client_error_line("打开日志目录失败: 权限不足");
        assert!(
            line.contains("[ERROR]"),
            "客户端自身错误应标记为 ERROR 级别，否则不会进入异常日志文件"
        );
        assert!(
            line.contains("[client]"),
            "来源应标注为 client，与插件转发的 background/sidepanel 等区分开"
        );
        assert!(
            line.contains("[client]") && line.matches("[client]").count() == 2,
            "插件名也应固定填 client，与来源字段语义一致，且日志查看页筛选插件名时能找到客户端自身日志"
        );
        assert!(
            line.contains("打开日志目录失败"),
            "应保留原始错误信息，否则排查时丢失具体原因"
        );
    }

    #[test]
    fn test_log_client_error_writes_to_registered_sink() {
        struct MemSink {
            lines: std::sync::Mutex<Vec<(String, bool)>>,
        }
        impl log_server::LogSink for MemSink {
            fn write_line(&self, line: &str, is_error: bool) {
                self.lines.lock().unwrap().push((line.to_string(), is_error));
            }
        }
        let sink = std::sync::Arc::new(MemSink {
            lines: std::sync::Mutex::new(Vec::new()),
        });
        // OnceLock 进程内只能设置一次；此测试独占 LOG_SINK 的首次写入验证
        let is_first_set = LOG_SINK.set(sink.clone() as std::sync::Arc<dyn log_server::LogSink>).is_ok();
        if !is_first_set {
            // 其它测试已抢先设置过（理论上不会，因本文件只有此处调用 set），跳过避免误判
            return;
        }

        log_client_error("测试错误消息");

        let lines = sink.lines.lock().unwrap();
        assert_eq!(
            lines.len(),
            1,
            "已注册 sink 时应写入其中，而非回落 eprintln!，否则打包后仍然看不到"
        );
        assert!(lines[0].1, "客户端自身错误应标记为异常，进入 aichat-error-*.log");
        assert!(lines[0].0.contains("测试错误消息"));
    }

    // ─────────────────────────────────────────────────────────
    // 启动版本上报：自更新失败为静默设计，靠这条判断哪台机器没升上来
    // ─────────────────────────────────────────────────────────

    #[test]
    fn test_build_startup_version_line_contains_version() {
        let msg = build_startup_version_line("0.2.0");
        assert!(
            msg.contains("0.2.0"),
            "启动日志必须含版本号，否则无法判断十几台机器里谁卡在旧版：{}",
            msg
        );
        assert!(
            msg.contains("客户端启动"),
            "需含固定前缀，便于在日志查看页按关键词筛出各机器的版本分布：{}",
            msg
        );
    }

    #[test]
    fn test_build_client_info_line_is_info_level_not_error() {
        let line = build_client_info_line("客户端启动，版本 0.2.0");
        assert!(
            line.contains("[INFO]"),
            "启动上报是普通信息而非异常，标成 ERROR 会污染「仅异常」视图：{}",
            line
        );
        assert!(
            line.contains("[client]"),
            "来源应标注 client，与插件转发的日志区分开：{}",
            line
        );
        assert!(
            !line.contains("[ERROR]"),
            "不应出现 ERROR 级别：{}",
            line
        );
    }

    // ─────────────────────────────────────────────────────────
    // 机器状态监测（DEV-124169）：采购同事怀疑虚拟机资源紧张导致
    // 插件卡顿，靠这组信息辅助判断。只测纯函数部分（字段映射、
    // 磁盘选择），真实采集依赖运行时系统状态，不易也不必单测
    // ─────────────────────────────────────────────────────────

    fn make_disk(mount_point: &str, total: u64, available: u64) -> DiskInfo {
        DiskInfo {
            mount_point: mount_point.to_string(),
            total_bytes: total,
            available_bytes: available,
        }
    }

    #[test]
    fn test_pick_disk_for_path_matches_longest_mount_prefix() {
        let disks = vec![
            make_disk("/", 100, 10),
            make_disk("/Users", 200, 20),
            make_disk("/Users/foo/data", 300, 30),
        ];
        // 安装路径落在最深的挂载点下，应选中最长前缀匹配的那个，
        // 而非第一个字面匹配到的 "/"
        let picked = pick_disk_for_path(&disks, Path::new("/Users/foo/data/aichat"));
        assert_eq!(
            picked.map(|d| d.mount_point.as_str()),
            Some("/Users/foo/data"),
            "应选中挂载点最长前缀匹配的磁盘，而非粗粒度的根盘"
        );
    }

    #[test]
    fn test_pick_disk_for_path_windows_drive_letter() {
        let disks = vec![make_disk("C:\\", 500, 50), make_disk("D:\\", 800, 80)];
        let picked = pick_disk_for_path(&disks, Path::new("D:\\aichat"));
        assert_eq!(
            picked.map(|d| d.mount_point.as_str()),
            Some("D:\\"),
            "虚拟机常见场景：安装在 D 盘时应选中 D 盘而非默认的 C 盘"
        );
    }

    #[test]
    fn test_pick_disk_for_path_falls_back_to_first_when_no_match() {
        let disks = vec![make_disk("/opt", 100, 10)];
        // 安装路径与任何已知挂载点都不匹配时，仍应返回一个可用磁盘作为
        // 保守估计，而不是让前端拿到空数据——总比完全没有磁盘信息有用
        let picked = pick_disk_for_path(&disks, Path::new("/completely/unrelated/path"));
        assert_eq!(
            picked.map(|d| d.mount_point.as_str()),
            Some("/opt"),
            "无匹配时应回退到列表中的第一个磁盘，而非返回 None"
        );
    }

    #[test]
    fn test_pick_disk_for_path_empty_list_returns_none() {
        let disks: Vec<DiskInfo> = vec![];
        assert!(
            pick_disk_for_path(&disks, Path::new("/any")).is_none(),
            "磁盘列表为空时应返回 None，而非 panic"
        );
    }

    #[test]
    fn test_build_system_snapshot_maps_fields_correctly() {
        let disks = vec![make_disk("/", 1_000_000_000, 400_000_000)];
        let snapshot = build_system_snapshot(
            HardwareSample {
                total_memory_bytes: 8_000_000_000,
                available_memory_bytes: 3_000_000_000,
                cpu_brand: "Apple M1".to_string(),
                cpu_cores: 8,
                cpu_usage_percent: 42.5,
                os_version: "macOS 14.5".to_string(),
            },
            &disks,
            Path::new("/aichat"),
        );
        assert_eq!(snapshot.total_memory_bytes, 8_000_000_000);
        assert_eq!(snapshot.available_memory_bytes, 3_000_000_000);
        assert_eq!(snapshot.cpu_brand, "Apple M1");
        assert_eq!(snapshot.cpu_cores, 8);
        assert_eq!(snapshot.cpu_usage_percent, 42.5);
        assert_eq!(snapshot.os_version, "macOS 14.5");
        assert_eq!(snapshot.disk_total_bytes, 1_000_000_000);
        assert_eq!(snapshot.disk_available_bytes, 400_000_000);
    }

    #[test]
    fn test_build_system_snapshot_disk_fields_zero_when_no_disk_found() {
        let disks: Vec<DiskInfo> = vec![];
        let snapshot = build_system_snapshot(
            HardwareSample {
                total_memory_bytes: 1,
                available_memory_bytes: 1,
                cpu_brand: "test".to_string(),
                cpu_cores: 1,
                cpu_usage_percent: 0.0,
                os_version: "test".to_string(),
            },
            &disks,
            Path::new("/aichat"),
        );
        assert_eq!(
            snapshot.disk_total_bytes, 0,
            "找不到磁盘时应给出 0 而非 panic，前端按 0 展示「未知」"
        );
        assert_eq!(snapshot.disk_available_bytes, 0);
    }
}
