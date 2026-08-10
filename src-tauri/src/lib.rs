use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{Read, Write};
use std::path::PathBuf;
use tauri::{Emitter, Manager};

pub mod log_file;
pub mod log_server;
pub mod updater_manifest;

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

/// 全局日志写入器：客户端自身的错误（如托盘操作失败）与插件转发日志
/// 共用同一套落盘文件，而非仅打到 stdout/stderr——打包后 stderr 无人接收，
/// 之前的 eprintln! 在生产环境等于没记录
static LOG_SINK: std::sync::OnceLock<std::sync::Arc<dyn log_server::LogSink>> =
    std::sync::OnceLock::new();

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

/// 刷新所有 Chrome 浏览器标签页
/// macOS 使用 AppleScript，Windows 使用 PowerShell
/// Chrome 未运行时不报错，直接返回跳过消息
#[tauri::command]
fn refresh_chrome_tabs() -> Result<String, String> {
    run_refresh_chrome_tabs_os()
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
            match log_server::spawn(sink, &app_version) {
                Ok(port) => {
                    LOG_SERVER_PORT.store(port, std::sync::atomic::Ordering::Relaxed);
                    println!(
                        "日志服务已启动: http://127.0.0.1:{}，日志目录: {:?}",
                        port,
                        get_log_dir()
                    );
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
}
