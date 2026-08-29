use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use tauri::{Emitter, Manager};

pub mod cli;
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
    /// 本机唯一标识（DEV-125123）。首次启动生成后不再变化。
    ///
    /// # 为什么现在就要
    /// 现在区分实例只靠 `plugin_name`（插件自报的下载目录名）——同机多实例够用
    /// （各实例目录不同、名字唯一），但十几台机器汇总时会撞名。规划文档把它列为
    /// 🟠 技术卡点并注明「不管推拉都要做，越晚做历史数据越脏」：不做的话，等真
    /// 接了集中上报再补，此前所有日志都无法归属到机器。
    #[serde(skip_serializing_if = "Option::is_none")]
    machine_id: Option<String>,
    /// 上次启动时记录的版本号（DEV-122552）。
    ///
    /// 存在的唯一理由是让启动日志能区分「刚升上来」与「只是重启」——
    /// 逐台核实一次 rollout 时，没有它就只能看到「版本 0.3.1」，
    /// 分不清这台是升级成功了还是本来就是这版。
    #[serde(skip_serializing_if = "Option::is_none")]
    last_version: Option<String>,
    /// 是否自动检查更新（2026-08-27）。**默认关**。
    ///
    /// # 为什么要有这个开关
    /// 测图标点击这类功能时，机器在背后自动升级会重启客户端、打断测试，
    /// 而且分不清问题是测试本身的还是升级造成的。
    ///
    /// # 默认关的代价（用户 2026-08-27 拍板，理由「点击一下就自动升级了」）
    /// 新装机器会**永远停在安装时那个版本**，除非有人点「立即检查更新」。
    /// 2026-08-24 刚踩过「自动更新坏了 6 天没人发现」，静默不升级同样难察觉，
    /// 故必须配显眼的界面提示——见巡检页顶部告警条。
    #[serde(skip_serializing_if = "Option::is_none")]
    auto_update: Option<bool>,
    /// 独立进程架构下，Chrome `--user-data-dir` 目录名 -> plugin_name
    /// 的映射（DEV-125986）。
    ///
    /// # 为什么需要人工维护这张表
    /// 真机实测确认：这类架构下，目录名（如 User4）与 plugin_name（如
    /// 10-4-LS10347）之间没有任何程序能读到的客观对应关系——它们是各自
    /// 独立的人工命名行为，三者（快捷方式名/目录名/plugin_name）编号
    /// 互相对不上。UI Automation 那条路径（单进程多 Profile 架构）不
    /// 需要这张表；只有这种「各实例各自 --user-data-dir 启动」的架构
    /// 才需要——人工在界面上做一次性选择确认（不需要手写 JSON），此后
    /// 除非该机器实例有变动才需要重新确认。
    ///
    /// 一次配置基本不变，跟 `machine_id` 同等对待，直接存进这份本机
    /// 配置文件，不新建文件、不新增读写机制。
    #[serde(skip_serializing_if = "Option::is_none")]
    chrome_profile_mapping: Option<std::collections::HashMap<String, String>>,
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

/// 读取自动检查更新偏好。
///
/// 配置缺失时返回 **false**（默认关，用户 2026-08-27 拍板）。
/// 与 autostart 的「默认关」取向一致，但代价不同：那个只是不改开机项，
/// 这个意味着不主动升级——发了修复版也要人点一下才生效。
pub fn load_auto_update_from_file(config_file: &PathBuf) -> bool {
    fs::read_to_string(config_file)
        .ok()
        .and_then(|c| serde_json::from_str::<PathConfig>(&c).ok())
        .and_then(|cfg| cfg.auto_update)
        .unwrap_or(false)
}

/// 写入自动检查更新偏好（保留其它字段不变）。
///
/// 必须保留其它字段：config.json 里还有安装路径、machine_id、last_version，
/// 抹掉 machine_id 等于换了台新机器、历史日志全部对不上。
pub fn save_auto_update_to_file(config_file: &PathBuf, enabled: bool) -> Result<(), String> {
    let mut cfg: PathConfig = if config_file.exists() {
        fs::read_to_string(config_file)
            .ok()
            .and_then(|c| serde_json::from_str(&c).ok())
            .unwrap_or_default()
    } else {
        PathConfig::default()
    };
    cfg.auto_update = Some(enabled);
    if let Some(parent) = config_file.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("创建配置目录失败: {}", e))?;
    }
    let content =
        serde_json::to_string_pretty(&cfg).map_err(|e| format!("序列化配置失败: {}", e))?;
    fs::write(config_file, content).map_err(|e| format!("写入配置文件失败: {}", e))
}

/// 读取独立进程架构的 Chrome Profile 映射表（DEV-125986）。
///
/// 配置缺失或未配置过时返回空表——空表是 [`resolve_lookup_strategy`]
/// 的合法输入，会让所有 plugin_name 都回退到 UI Automation 路径，
/// 不需要特殊处理「没有映射表」这个情况。
pub fn load_chrome_profile_mapping(
    config_file: &PathBuf,
) -> std::collections::HashMap<String, String> {
    fs::read_to_string(config_file)
        .ok()
        .and_then(|c| serde_json::from_str::<PathConfig>(&c).ok())
        .and_then(|cfg| cfg.chrome_profile_mapping)
        .unwrap_or_default()
}

/// 写入 Chrome Profile 映射表（保留其它字段不变），整体覆盖而非合并。
///
/// 整体覆盖是有意为之：人工每次重新走确认界面，应该以那一轮的结果为
/// 准——合并会让已下线实例的旧映射永久残留，误导后续查询。
pub fn save_chrome_profile_mapping(
    config_file: &PathBuf,
    mapping: &std::collections::HashMap<String, String>,
) -> Result<(), String> {
    let mut cfg: PathConfig = if config_file.exists() {
        fs::read_to_string(config_file)
            .ok()
            .and_then(|c| serde_json::from_str(&c).ok())
            .unwrap_or_default()
    } else {
        PathConfig::default()
    };
    cfg.chrome_profile_mapping = Some(mapping.clone());
    if let Some(parent) = config_file.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("创建配置目录失败: {}", e))?;
    }
    let content =
        serde_json::to_string_pretty(&cfg).map_err(|e| format!("序列化配置失败: {}", e))?;
    fs::write(config_file, content).map_err(|e| format!("写入配置文件失败: {}", e))
}

/// 读取或首次生成本机标识（DEV-125123）。
///
/// 已有则原样返回；没有则生成并落盘。**生成后不再变化**——它的用途是把日志
/// 归属到机器，一变就等于换了台新机器，历史数据全部对不上。
///
/// 形态是 `<主机名>-<随机后缀>`：只用主机名不行（虚拟机克隆出来会重名，
/// 而采购机大概率就是克隆的），只用随机串则人看不出是哪台、排查时无从下手。
pub fn load_or_create_machine_id(config_file: &PathBuf, hostname: &str) -> String {
    let mut cfg: PathConfig = if config_file.exists() {
        fs::read_to_string(config_file)
            .ok()
            .and_then(|c| serde_json::from_str(&c).ok())
            .unwrap_or_default()
    } else {
        PathConfig::default()
    };
    if let Some(existing) = cfg.machine_id.as_ref().filter(|s| !s.is_empty()) {
        return existing.clone();
    }

    let id = build_machine_id(hostname, &ws_token::generate_token());
    cfg.machine_id = Some(id.clone());
    // 写失败不影响本次运行，只是下次启动会重新生成——比因为写不了配置
    // 就拒绝启动要好（采购机可能有权限问题）
    if let Some(parent) = config_file.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(content) = serde_json::to_string_pretty(&cfg) {
        let _ = fs::write(config_file, content);
    }
    id
}

/// 拼装机器 ID（纯函数）。主机名做安全化处理，随机部分取前 8 位。
///
/// 主机名里的空格、中文、标点会让 ID 在日志行、文件名、URL 里都不好处理，
/// 故只保留字母数字与横杠，其余替换为 `-`
pub fn build_machine_id(hostname: &str, random_hex: &str) -> String {
    let safe: String = hostname
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    // 连续横杠压成一个，并去掉首尾横杠，避免出现 `--host---1a2b`
    let mut host = String::new();
    let mut prev_dash = false;
    for c in safe.chars() {
        if c == '-' {
            if !prev_dash {
                host.push(c);
            }
            prev_dash = true;
        } else {
            host.push(c);
            prev_dash = false;
        }
    }
    let host = host.trim_matches('-');
    let host = if host.is_empty() { "unknown" } else { host };
    // 主机名过长会让 ID 难读，截断到 32 字符
    let host: String = host.chars().take(32).collect();
    let suffix: String = random_hex.chars().take(8).collect();
    format!("{}-{}", host, suffix)
}

/// 取本机主机名，取不到时回落 `unknown`
fn current_hostname() -> String {
    // 优先环境变量（Windows 用 COMPUTERNAME，Unix 用 HOSTNAME）
    for key in ["COMPUTERNAME", "HOSTNAME"] {
        if let Ok(v) = std::env::var(key) {
            if !v.is_empty() {
                return v;
            }
        }
    }
    // 回落到 hostname 命令：Unix 上 HOSTNAME 常未导出到子进程环境
    #[cfg(not(target_os = "windows"))]
    {
        if let Ok(out) = std::process::Command::new("hostname").output() {
            let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !s.is_empty() {
                return s;
            }
        }
    }
    "unknown".to_string()
}

/// 取本机标识，供日志与查询输出携带
pub fn get_machine_id() -> String {
    load_or_create_machine_id(
        &get_config_file_path_with_dir(&get_app_config_dir()),
        &current_hostname(),
    )
}

/// 供 log_server 的只读接口使用的日志目录（`get_log_dir` 是私有的）
pub fn get_log_dir_public() -> PathBuf {
    get_log_dir()
}

/// 今天的日期（北京时间），格式 YYYY-MM-DD
pub fn current_date() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

/// 本机对外自报的机器名（DEV-125123）。
///
/// # 为什么用插件上报的名字而不是主机名
/// 开发实际是按「10-2-cj055514」这类编号找机器的——那是插件的下载目录名
/// （见 pms-aichat 的 pluginIdentity），每台机器不同且稳定，团队本来就用它
/// 指代机器。用自动生成的 machine_id 或系统主机名，查日志时还得多一层映射。
///
/// 取值来自心跳注册表里最近上报过的插件名。取不到（插件还没上报过）时
/// 回落 machine_id，保证这个字段永远有值——AI 靠它确认「连对机器了吗」。
pub fn get_reported_machine_name() -> String {
    if let Some(reg) = HEARTBEATS.get() {
        if let Ok(guard) = reg.lock() {
            // 多实例时取最近活跃的那个：它们同属一台机器，名字前缀相同，
            // 拿哪个都能标识这台机器
            let snaps = guard.snapshots(std::time::Instant::now());
            if let Some(first) = snaps.iter().min_by_key(|s| s.silence_secs) {
                return first.plugin_name.clone();
            }
        }
    }
    get_machine_id()
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
    // 同样必须走 powershell_no_window。频率虽低（只在更新插件后跑一次），
    // 但那个时机机器正在干活，弹窗一样抢焦点
    let output = powershell_no_window(&cmd)
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

/// 执行平台相关的「打开插件侧边栏」命令。
///
/// **已无调用方**（2026-08-24 移除了唯一入口）：它必须先 AppActivate 抢全局
/// 焦点才能 SendKeys，会打断那台机器上正在往供应商聊天框输入的实例。
/// 保留代码与测试是为了记录「为什么这条路走不通」，避免后人重新踩一遍。
#[allow(dead_code)]
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
    // 走 powershell_no_window：本函数虽已停用，但接线时若漏了这个标志
    // 就会重蹈 2026-08-21 的覆辙（弹窗抢焦点打断插件输入）
    let output = powershell_no_window(&cmd)
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

/// 执行平台相关的「重启 Chrome」命令。
///
/// **已无调用方**：二级自愈从未接线（多开 15 个实例时重启会把全部干掉，
/// 见 DEV-124837），且其配套的开侧边栏动作会抢焦点。
#[allow(dead_code)]
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
    // 走 powershell_no_window：本函数虽已停用，但接线时若漏了这个标志
    // 就会重蹈 2026-08-21 的覆辙（弹窗抢焦点打断插件输入）
    let output = powershell_no_window(&cmd)
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

/// 本次运行的 WS 握手令牌。插件需带上它才能建立 WS 连接——
/// WS 不受同源策略限制，任意网页脚本都能连本机端口，故必须校验
static WS_TOKEN: std::sync::OnceLock<String> = std::sync::OnceLock::new();

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
///
/// # 为什么要带上一版本
/// 逐台核实一次 rollout 时，只看「启动，版本 0.3.1」分不清这台是**刚升上来的**
/// 还是**本来就是这版**。带上变化前的版本，升级成功与否一眼可判；版本没变时
/// 不加这段，避免每次重启都被读成一次升级（那会让核实结果全是假阳性）。
pub fn build_startup_version_line(version: &str, previous: Option<&str>) -> String {
    match previous {
        Some(prev) if prev != version => {
            format!("客户端启动，版本 {}（升级前 {}）", version, prev)
        }
        _ => format!("客户端启动，版本 {}", version),
    }
}

/// 读取上次记录的版本号，并把当前版本写回配置。
///
/// 返回上次记录的版本（首次运行为 `None`）。供启动日志区分「升级」与「重启」。
/// 复用 `PathConfig` 的 `#[serde(flatten)] extra` 兜底，加字段不会抹掉
/// 安装路径与 machine_id —— 后者一变就等于换了台新机器，历史日志全部对不上。
pub fn load_and_record_version(config_file: &PathBuf, current: &str) -> Option<String> {
    let mut cfg: PathConfig = if config_file.exists() {
        fs::read_to_string(config_file)
            .ok()
            .and_then(|c| serde_json::from_str(&c).ok())
            .unwrap_or_default()
    } else {
        PathConfig::default()
    };
    let previous = cfg.last_version.take().filter(|s| !s.is_empty());
    cfg.last_version = Some(current.to_string());
    if let Some(parent) = config_file.parent() {
        let _ = fs::create_dir_all(parent);
    }
    // 写失败只影响下次启动能否说出「升级前是哪版」，不该阻断启动
    if let Ok(content) = serde_json::to_string_pretty(&cfg) {
        let _ = fs::write(config_file, content);
    }
    previous
}

// ─────────────────────────────────────────────────────────────────
// 自更新可观测性（DEV-122551 / DEV-122552）
//
// 十几台采购机升一次的代价很高（自动升级链路本身就是坏的，见下），所以
// 「排查这条链路需要的日志」必须在升级前一次埋够，而不是升上去再发现少了字段。
//
// 2026-08-24 的实测教训：那天客户端每 4 小时报一次
// `自动更新失败: Download request failed with status: 500`，日志里除了这句
// 什么都没有 —— 没有地址、没有响应体、没有耗时，成功时更是一个字不写。
// 根因（服务端对 `Accept: application/octet-stream` 整站返回 500）是靠人反复
// curl 对比才定位的。下面这批日志的目标就是让同类问题在日志里自己说出结论。
// ─────────────────────────────────────────────────────────────────

/// 同一条日志在时间窗内只记一次。
///
/// # 为什么不用「只记一次、直到成功才解除」
/// 那是前端原来的写法（`lastSelfUpdateError`）。它在**持续失败**下会永久静默：
/// 断网的机器 `check()` 每次都失败、永远等不到那次「成功」来解除抑制，于是第一条
/// 之后再也不记。排查者看到日志里没有异常，会误判成「这台机器没问题」——
/// 而这恰好是最需要发现的状态。
///
/// 按时间窗抑制则保证「一直坏就一直有记录」，只是把频率压到可接受。
pub struct LogThrottle {
    last: std::collections::HashMap<String, std::time::Instant>,
    window: std::time::Duration,
}

impl LogThrottle {
    pub fn new(window: std::time::Duration) -> Self {
        Self {
            last: std::collections::HashMap::new(),
            window,
        }
    }

    /// 判断此刻是否应该记录 `key` 这条日志。返回 true 时同步刷新计时。
    pub fn should_log(&mut self, key: &str, now: std::time::Instant) -> bool {
        match self.last.get(key) {
            Some(&prev) if now.saturating_duration_since(prev) < self.window => false,
            _ => {
                self.last.insert(key.to_string(), now);
                true
            }
        }
    }
}

/// 每次自更新检查完成后都记一条 —— **成功也记**。
///
/// # 为什么成功也要记
/// 原先只在失败时写日志，于是「这台机器还在检查更新吗」根本判断不了：
/// 定时器死了、进程僵住、和一切正常，在日志里长得一模一样（都是没有日志）。
/// 每 4 小时一条的节律本身就是心跳 —— 超过一个周期没有这条，就是客户端出问题了。
/// `trigger` 区分「定时轮询」与「用户点了按钮」。
///
/// # 为什么这个字段不能省
/// 这条日志被当作 4 小时一次的存活心跳用 —— 而 2026-08-24 在真机上一次
/// 连点「检查更新」就刷了 10 条，全都长得一模一样。分不清来源的话，
/// 一串手动点击会把节律搞乱，「这台还活着吗」的判断就不准了。
pub fn build_self_update_check_line(
    trigger: &str,
    current: &str,
    remote: &str,
    verdict: &str,
    elapsed_ms: u128,
) -> String {
    format!(
        "[自更新] 检查完成 触发={} 当前={} 远端={} 结论={} 耗时={}ms",
        trigger, current, remote, verdict, elapsed_ms
    )
}

/// 自更新某个阶段失败时记录完整上下文。
///
/// `stage` 形如「检查」「下载」「安装」；`detail` 承载状态码、响应体片段等。
/// 地址必须带上：指错环境（测试包发到线上机）时，靠它才能一眼看出来。
pub fn build_self_update_failure_line(
    stage: &str,
    url: &str,
    detail: &str,
    elapsed_ms: u128,
) -> String {
    format!(
        "[自更新] {}失败 url={} {} 耗时={}ms",
        stage, url, detail, elapsed_ms
    )
}

/// 把字节数格式化成人类可读形式。采购机上的日志也可能被人直接打开看
pub fn format_bytes(bytes: u64) -> String {
    const MB: f64 = 1024.0 * 1024.0;
    const KB: f64 = 1024.0;
    let b = bytes as f64;
    if b >= MB {
        format!("{:.1}MB", b / MB)
    } else if b >= KB {
        format!("{:.0}KB", b / KB)
    } else {
        format!("{}B", bytes)
    }
}

/// 下载过程中的里程碑日志。
///
/// # 为什么需要进度而不只是结果
/// 原先只在下载**完成后**记一次总量，于是「卡住」这种状态完全看不出来 ——
/// 而卡住（不是报错、就是不动）恰好是最难现场排查的一种：界面停在「正在下载」，
/// 日志里既没有失败也没有完成，无从判断是没开始、下到一半断了、还是很慢。
///
/// `total` 为 0 表示服务端没给 Content-Length，此时不输出百分比 ——
/// 凭空算一个假进度比不给更糟。
pub fn build_download_progress_line(received: u64, total: u64, elapsed_ms: u128) -> String {
    let speed = if elapsed_ms > 0 {
        let bps = (received as f64) * 1000.0 / (elapsed_ms as f64);
        format!(" 速度={}/s", format_bytes(bps as u64))
    } else {
        String::new()
    };
    if total > 0 {
        format!(
            "[自更新] 下载中 {}% {}/{}{}",
            received * 100 / total,
            format_bytes(received),
            format_bytes(total),
            speed
        )
    } else {
        format!(
            "[自更新] 下载中 {}（总量未知）{}",
            format_bytes(received),
            speed
        )
    }
}

/// 根据「带 Accept」与「不带 Accept」两次探测的结果给出结论。
///
/// # 这条为什么值得埋
/// 2026-08-24 的根因是服务端对 `Accept: application/octet-stream` 返回 500，
/// 而同一地址不带该头返回 200 —— 定位过程是人手动 curl 对比出来的。
/// 把这个对比做成失败路径上的自动探针，同类问题下次会在日志里直接给出结论，
/// 不必再有人去猜是网络、是服务端、还是请求头。
///
/// 探针对**任何**「服务端挑请求」的故障都有效，不是只针对这一次。
pub fn build_diagnosis_verdict(with_accept: Option<u16>, without_accept: Option<u16>) -> String {
    match (with_accept, without_accept) {
        (None, None) => {
            "两种请求都发不出去 ⇒ 网络不通或 DNS 解析失败，不是服务端拒绝".to_string()
        }
        (Some(bad), Some(ok)) if bad >= 400 && ok < 400 => format!(
            "带 Accept: application/octet-stream 返回 {}、不带返回 {} ⇒ 服务端按请求头拒绝，非网络故障",
            bad, ok
        ),
        (Some(a), Some(b)) if a < 400 && b < 400 => format!(
            "诊断时两种请求都正常（{} / {}）⇒ 瞬时故障，重试即可",
            a, b
        ),
        (Some(a), Some(b)) => format!("带 Accept={}、不带={} ⇒ 服务端两种都拒，检查地址是否有效", a, b),
        (Some(a), None) => format!("带 Accept={}，不带 Accept 请求发不出去 ⇒ 结果不可靠，建议重试", a),
        (None, Some(b)) => format!(
            "带 Accept 请求发不出去、不带返回 {} ⇒ 疑似请求头触发中间设备拦截",
            b
        ),
    }
}

/// 从 Tauri 的插件配置里取出自更新清单地址。
///
/// 不写成常量是为了**不可能漂移**：真实生效的地址在 `tauri.conf.json` 的
/// `plugins.updater.endpoints`，另抄一份常量迟早会和它不一致，而那种不一致最坑 ——
/// 日志上报的地址与实际请求的地址不同，排查者会朝着错误方向查很久。
///
/// 取不到时返回「未配置」而非 panic：这只是日志里的一个字段，
/// 不该因为读不到配置就影响启动。
pub fn extract_updater_endpoint(plugins: &serde_json::Value) -> String {
    plugins
        .get("updater")
        .and_then(|u| u.get("endpoints"))
        .and_then(|e| e.as_array())
        .and_then(|a| a.first())
        .and_then(|v| v.as_str())
        .unwrap_or("未配置")
        .to_string()
}

/// 构建放行日志端口的防火墙规则命令（纯函数，可测）。
///
/// # 为什么客户端自己也试一次
/// NSIS 安装钩子只在有管理员权限时才能加规则，而**自动升级时更新器不是管理员**，
/// 那条钩子不会生效。端口没放行的后果很具体：AI 读不到那台机器的日志，
/// 于是连「它到底升没升」都无从确认 —— 日志埋得再全也救不了读取通道本身被挡住。
///
/// 客户端启动时尽力试一次：成了最好，没权限也把失败原因记进日志（本机可读）。
/// 只放行入站；出站本来就不受限，多加规则等于无谓扩大暴露面。
pub fn build_firewall_rule_script_windows(port: u16) -> String {
    format!(
        "netsh advfirewall firewall add rule name=\"aichat-updater\" \
         dir=in action=allow protocol=TCP localport={}",
        port
    )
}

/// 包一层 PowerShell，把 netsh 的输出转成 UTF-8 再交给我们。
///
/// # 为什么需要这层转换
/// 2026-08-24 真机（中文 Windows）实测，这条日志长这样：
///   `退出码=1 stdout=����Ĳ�����Ҫ����(��Ϊ����Ա����)��`
/// 原文是「请求的操作需要提升(作为管理员运行)」——netsh 按控制台代码页
/// （中文系统是 GBK/936）输出，而 Rust 侧按 UTF-8 解，于是全成了替换字符。
///
/// 这次靠猜能还原（因为已经知道是权限问题），但**下次遇到没见过的错误就真读不出来**
/// ——而这条日志正是防火墙盲区的唯一仪表，读不出等于没有。
///
/// 做法：让 PowerShell 先用默认（OEM）编码正确地把 netsh 输出读成字符串，
/// 再把自身输出编码切成 UTF-8 后写出。顺序不能反 —— 先切成 UTF-8 会让
/// PowerShell 用 UTF-8 去解 GBK 字节，反而更糟。
///
/// `$LASTEXITCODE` 必须在切编码之前取：中间任何一条命令都会覆盖它。
/// 构建调用图标定位脚本的参数（纯函数，可测）。
///
/// 对应 `docs/chatgpt-icon-locator/README.md` 里推荐的「方案 A，一步到位」：
/// `--activate --click` —— 激活 Chrome、找到图标、直接点。
///
/// # 为什么用 --activate
/// 脚本靠**整屏截图**做模板匹配，目标窗口被遮挡时截图上根本没有那个图标。
/// `--activate` 先把它抬到最前，是识别得以成立的前提，不是可选项。
///
/// # 已知代价（领导方案的既有属性，接入时如实保留）
/// `--activate` 内部会敲 Alt 越过前台锁 + SetForegroundWindow 抢前台，
/// `--click` 会 SetCursorPos 移动真实鼠标 + mouse_event 物理点击。
/// 一台机器跑 8~10 个实例时，抢焦点会打断其它实例正在进行的聊天输入。
///
/// # 精确定位（DEV-125986）
/// 传入 `target` 时，改用 `--hwnd`（精确指定要激活的窗口，替代模糊的
/// `--activate-match` 标题匹配）+ `--region`（把匹配范围收窄到该窗口，
/// 替代全屏扫描）。`target` 为 `None` 时保持原有全屏 + 标题匹配行为，
/// 用于识别路径两条都没能定位到目标窗口时的兜底（宁可按旧逻辑赌一次，
/// 也不是直接放弃不点）。
///
/// # 为什么返回参数数组而不是拼一整条字符串（2026-08-29 真机 bug 修复）
/// 旧实现拼一整条字符串交给 `cmd /C "python <整条字符串>"` 执行。真机实测
/// 暴露的问题：Tauri `path().resolve()` 在某些场景下返回的脚本路径带
/// `\\?\` 长路径前缀，混进这条字符串后被 `cmd` 的引号/转义规则打乱，
/// 实际执行时报 `can't open file`，路径被拼成相对于
/// `C:\Windows\system32` 的畸形结果——报错还被上层误判成「找不到图标」，
/// 完全文不对题。返回参数数组交给 `Command::args()` 直接执行，每个参数
/// 各自独立传递，不经过任何 shell 字符串解析，从根上避免这整类问题。
pub fn build_icon_click_command(script_path: &str, target: Option<&IconClickTarget>) -> Vec<String> {
    let mut args = vec![script_path.to_string(), "--activate".to_string()];
    match target {
        Some(t) => {
            args.push("--hwnd".to_string());
            args.push(t.hwnd.to_string());
            args.push("--region".to_string());
            args.push(format!(
                "{},{},{},{}",
                t.region.0, t.region.1, t.region.2, t.region.3
            ));
        }
        None => {
            args.push("--activate-match".to_string());
            args.push("Google Chrome".to_string());
        }
    }
    args.push("--click".to_string());
    args
}

/// 精确点击目标：已定位到的窗口句柄 + 该窗口在屏幕上的矩形（虚拟桌面
/// 绝对坐标，(left, top, width, height)，与 Win32 `GetWindowRect` 同
/// 坐标系）。有了这两项，脚本不再需要全屏扫描 + 标题匹配去猜。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IconClickTarget {
    pub hwnd: u64,
    pub region: (i32, i32, i32, i32),
}

/// 解析图标定位脚本的执行结果（纯函数，可测）。
///
/// 脚本的输出契约（见其 README）：
///
/// | 情况 | stdout | 退出码 |
/// |---|---|---|
/// | 找到 | 一行 `x,y`（物理像素绝对坐标） | 0 |
/// | 没找到（置信度低于阈值） | 空，stderr 打印 NOT_FOUND | 2 |
/// | 出错（模板缺失/无 Chrome 窗口） | 空，stderr 打印 ERROR | 1 |
///
/// stdout 解析不出坐标时返回错误而非 `(0, 0)`：后者会让调用方以为点成功了，
/// 而实际坐标指向屏幕左上角。
pub fn parse_icon_click_result(
    stdout: &str,
    exit_code: i32,
    stderr: &str,
) -> Result<(i32, i32), String> {
    match exit_code {
        0 => {
            let s = stdout.trim();
            let (x, y) = s
                .split_once(',')
                .ok_or_else(|| format!("脚本输出无法解析为坐标: {:?}", s))?;
            let x: i32 = x
                .trim()
                .parse()
                .map_err(|_| format!("横坐标不是整数: {:?}", x))?;
            let y: i32 = y
                .trim()
                .parse()
                .map_err(|_| format!("纵坐标不是整数: {:?}", y))?;
            Ok((x, y))
        }
        2 => Err(format!(
            "未找到插件图标（置信度低于阈值）。可能是图标改版、分辨率变化，\
             需在该机器上重新截取模板图。脚本输出: {}",
            stderr.trim()
        )),
        1 => Err(format!(
            "脚本执行出错（模板文件缺失，或找不到 Chrome 窗口）: {}",
            stderr.trim()
        )),
        // Windows 标准错误码：cmd 找不到要执行的命令。这里几乎总是因为
        // 目标机器没装 Python、或装了但没加入系统 PATH（2026-08-29 真机
        // 实测踩过）——直接点出根因，而不是让非技术的采购同事去查一个
        // 陌生的数字含义
        9009 => Err(
            "未找到 python 命令。该机器可能未安装 Python，或安装时未勾选\
             「Add python.exe to PATH」。请安装 Python 3.8 及以上版本\
             （勾选加入 PATH）并安装依赖（opencv-python、numpy、Pillow）后重试"
                .to_string(),
        ),
        c => Err(format!("脚本返回未知退出码 {}: {}", c, stderr.trim())),
    }
}

pub fn build_firewall_powershell_script(port: u16) -> String {
    format!(
        "$ErrorActionPreference='SilentlyContinue'; \
         netsh advfirewall firewall delete rule name=\"aichat-updater\" | Out-Null; \
         $out = {} 2>&1 | Out-String; \
         $code = $LASTEXITCODE; \
         [Console]::OutputEncoding = [System.Text.Encoding]::UTF8; \
         Write-Output $out; \
         exit $code",
        build_firewall_rule_script_windows(port)
    )
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

/// 记录一次自更新检查的结果（**成功也记**）。
///
/// 见 `build_self_update_check_line` 的文档：这条同时充当客户端存活心跳，
/// 没有它就无法区分「定时器死了」和「一切正常」。
#[tauri::command]
fn log_self_update_check(
    trigger: String,
    current: String,
    remote: String,
    verdict: String,
    elapsed_ms: u64,
) {
    log_client_info(&build_self_update_check_line(
        &trigger,
        &current,
        &remote,
        &verdict,
        elapsed_ms as u128,
    ));
}

/// 记录自更新某阶段失败的完整上下文（地址、状态、耗时）。
#[tauri::command]
fn log_self_update_failure(stage: String, url: String, detail: String, elapsed_ms: u64) {
    log_client_error(&build_self_update_failure_line(
        &stage,
        &url,
        &detail,
        elapsed_ms as u128,
    ));
}

/// 记录下载进度里程碑。INFO 级，不进异常文件。
#[tauri::command]
fn log_self_update_progress(received: u64, total: u64, elapsed_ms: u64) {
    log_client_info(&build_download_progress_line(
        received,
        total,
        elapsed_ms as u128,
    ));
}

/// 下载失败后对同一地址做一次对比探测，把结论写进日志。
///
/// 发两个 HEAD：一个带 `Accept: application/octet-stream`（复现 updater 的行为）、
/// 一个不带。两者结果的差异直接指向根因类别 —— 详见 `build_diagnosis_verdict`。
///
/// # 为什么在失败路径上再发网络请求是值得的
/// 只在失败后触发，频率被自更新周期（4 小时）天然限制；而它换来的是
/// 「日志自己说出根因类别」。2026-08-24 那次，人靠手动 curl 对比才定位到
/// 是请求头问题，中间一度怀疑过网络、CDN、文件损坏 —— 那些弯路这条探针能省掉。
#[tauri::command]
async fn diagnose_download_url(url: String) -> String {
    let verdict = match probe_download_url(&url).await {
        Ok((with_accept, without_accept)) => {
            build_diagnosis_verdict(with_accept, without_accept)
        }
        Err(e) => format!("诊断探针无法创建 HTTP 客户端: {}", e),
    };
    log_client_error(&format!("[自更新] 自诊断 url={} {}", url, verdict));
    verdict
}

/// 对同一地址发两次 HEAD：一次带 `Accept: application/octet-stream`（复现
/// updater 的行为）、一次不带。返回各自的状态码（发不出去为 `None`）。
///
/// 与 `diagnose_download_url` 分开是为了能对真实服务端做集成验证 ——
/// 判断逻辑（`build_diagnosis_verdict`）有单测，但「服务端真的会这么回吗」
/// 只有打一次真实请求才算证实。
pub async fn probe_download_url(url: &str) -> Result<(Option<u16>, Option<u16>), String> {
    // 探针本身不该因为证书或代理问题失败得比被诊断的请求更早，
    // 故沿用与插件下载同一套客户端构建方式（线上环境不放宽任何校验）
    let client = build_http_client("online").map_err(|e| e.to_string())?;
    let probe = |accept: Option<&'static str>| {
        let client = client.clone();
        let url = url.to_string();
        async move {
            let mut req = client.head(&url);
            if let Some(a) = accept {
                req = req.header(reqwest::header::ACCEPT, a);
            }
            req.send().await.ok().map(|r| r.status().as_u16())
        }
    };
    let with_accept = probe(Some("application/octet-stream")).await;
    let without_accept = probe(None).await;
    Ok((with_accept, without_accept))
}

/// 尽力放行日志服务端口。
///
/// 详见 `build_firewall_rule_script_windows` 的文档：自动升级时更新器不是管理员，
/// NSIS 安装钩子不会生效，所以客户端自己再试一次。**失败是常态**（非管理员），
/// 失败原因照样记进日志 —— 那份日志在本机可读，人上机排查时能立刻看到症结。
fn try_add_firewall_rule(port: u16) {
    match try_add_firewall_rule_os(port) {
        Ok(msg) => log_client_info(&format!("[启动] 防火墙放行 {} 端口成功: {}", port, msg)),
        // INFO 而非 ERROR：非管理员运行时必然失败，记成异常会让「仅异常」视图
        // 每次启动都多一条噪音，而这不是故障
        Err(e) => log_client_info(&format!(
            "[启动] 防火墙放行 {} 端口未成功（多为非管理员运行，需人工执行一次 netsh）: {}",
            port, e
        )),
    }
}

/// 构建一个**不弹控制台窗口**的 PowerShell 调用。
///
/// # 为什么必须统一走这里
/// 少加 `CREATE_NO_WINDOW` 的后果不是「界面难看」，而是**抢走前台焦点** ——
/// 而焦点是全局唯一资源：一台采购机上跑着 8~10 个 Chrome 实例，插件正在往
/// 供应商聊天框输入文字，焦点被抢走会让后续按键落到别处、或者输入丢字，
/// 把发出去的聊天内容弄脏。
///
/// 2026-08-21 正是因为这个原因砍掉了「自动拉起侧边栏」整条路径，
/// 但 2026-08-25 在真机上发现巡检页仍在弹窗：`list_chrome_windows_os` 漏了这个
/// 标志，而巡检页每 5 秒刷新一次 —— 等于每 5 秒抢一次焦点，比当初砍掉的那条
/// 路径还频繁。同一个坑从另一个地方漏了进来。
///
/// 故把标志收进本函数，新增 PowerShell 调用一律用它，不要再各自 `Command::new`。
#[cfg(target_os = "windows")]
fn powershell_no_window(script: &str) -> std::process::Command {
    use std::os::windows::process::CommandExt;
    // Win32 CREATE_NO_WINDOW：子进程不分配控制台窗口，因而不会抢焦点
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let mut cmd = std::process::Command::new("powershell");
    cmd.args(["-NoProfile", "-NonInteractive", "-Command", script])
        .creation_flags(CREATE_NO_WINDOW);
    cmd
}

#[cfg(target_os = "windows")]
fn try_add_firewall_rule_os(port: u16) -> Result<String, String> {
    // 规则同名时 netsh 会再加一条重复规则而不报错，故先删后加保持幂等。
    // 编码转换见 build_firewall_powershell_script 的文档
    let script = build_firewall_powershell_script(port);
    let output = powershell_no_window(&script)
        .output()
        .map_err(|e| e.to_string())?;
    let detail = describe_command_output(
        output.status.code(),
        &String::from_utf8_lossy(&output.stdout),
        &String::from_utf8_lossy(&output.stderr),
    );
    if output.status.success() {
        Ok(detail)
    } else {
        Err(detail)
    }
}

/// 把命令执行结果汇总成一句可读说明。
///
/// # 为什么不能只取 stderr
/// 2026-08-24 在真机（DESKTOP-MQUBUQS）实测：netsh 失败时这条日志的冒号后面
/// **是空的** —— 因为 netsh 把「拒绝访问」之类的说明写到 **stdout** 而不是
/// stderr，只取 stderr 就得到空串。结果这条日志只能说明「失败了」，
/// 说不出为什么，而它恰恰是防火墙盲区的唯一仪表：端口不通就读不到日志，
/// 读不到日志就无法确认那台机器升没升。
///
/// 所以 stdout / stderr / 退出码三者都带上，并在全空时明说「无输出」，
/// 而不是留一个让人以为日志被截断了的空白。
pub fn describe_command_output(code: Option<i32>, stdout: &str, stderr: &str) -> String {
    let mut parts = Vec::new();
    let out = stdout.trim();
    let err = stderr.trim();
    if !out.is_empty() {
        parts.push(format!("stdout={}", out));
    }
    if !err.is_empty() {
        parts.push(format!("stderr={}", err));
    }
    if parts.is_empty() {
        parts.push("无输出".to_string());
    }
    match code {
        Some(c) => format!("退出码={} {}", c, parts.join(" ")),
        None => format!("退出码=未知(被信号终止) {}", parts.join(" ")),
    }
}

#[cfg(not(target_os = "windows"))]
fn try_add_firewall_rule_os(_port: u16) -> Result<String, String> {
    // macOS/Linux 上开发调试用，不做防火墙改动：采购机全是 Windows，
    // 在开发机上动系统防火墙没有收益、只有风险
    Err("非 Windows 平台，跳过".to_string())
}

/// 获取开机自启偏好，供前端/托盘菜单回显开关状态
/// 供前端读取自动更新开关，决定要不要起轮询定时器
#[tauri::command]
fn get_auto_update() -> bool {
    load_auto_update_from_file(&get_runtime_config_file())
}

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

// 「打开插件侧边栏」命令已于 2026-08-24 移除（连同前端按钮）。
// 它是客户端里最后一个会抢全局焦点的入口——实现只能靠 AppActivate + SendKeys，
// 而焦点是全局唯一资源：一台机器跑 3~15 个 Chrome 实例、插件正通过它们往供应商
// 聊天框输入文字，抢一次焦点就会打断其中正在输入的那个（不限于目标实例），
// 按键可能落进聊天框造成乱字符或丢字，污染发给供应商的内容。
//
// 下面的 build_open_sidepanel_* / run_open_sidepanel_os 保留但已无调用方：
// 它们记录了「为什么这条路走不通」（Chrome 强制 sidePanel.open() 由用户手势
// 触发，模拟按键是唯一途径而它必须抢焦点），有测试守着这些约束，删掉会让后人
// 重新踩一遍。要恢复该能力，必须先解决抢焦点问题。

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

/// Chrome Profile 映射表确认界面用的候选数据（DEV-125986）。
///
/// 独立进程架构下，目录名与 plugin_name 之间无法自动推导对应关系，
/// 需要人工在界面上做一次性选择确认——本结构就是界面要展示的两列，
/// 加上已经保存过的映射（用于预填，不用每次从头选）。
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ChromeProfileCandidates {
    /// 当前检测到的 chrome.exe 主进程 user-data-dir 目录名
    directory_names: Vec<String>,
    /// 当前心跳在线的 plugin_name
    online_plugin_names: Vec<String>,
    /// 已保存的映射（目录名 -> plugin_name），用于界面预填
    saved_mapping: std::collections::HashMap<String, String>,
}

/// 读取映射表确认界面所需的候选数据。
#[tauri::command]
fn get_chrome_profile_candidates() -> Result<ChromeProfileCandidates, String> {
    let directory_names = list_chrome_user_data_dir_names_os().unwrap_or_default();
    let online_plugin_names = match HEARTBEATS.get() {
        Some(reg) => match reg.lock() {
            Ok(r) => r
                .snapshots(std::time::Instant::now())
                .into_iter()
                .map(|s| s.plugin_name)
                .collect(),
            Err(_) => Vec::new(),
        },
        None => Vec::new(),
    };
    let saved_mapping = load_chrome_profile_mapping(&get_runtime_config_file());
    Ok(ChromeProfileCandidates {
        directory_names,
        online_plugin_names,
        saved_mapping,
    })
}

/// 保存人工在映射表确认界面里选定的对应关系。
///
/// 整体覆盖（见 [`save_chrome_profile_mapping`] 文档）：每次都以这一轮
/// 确认的结果为准，不与历史映射合并。
#[tauri::command]
fn save_chrome_profile_mapping_cmd(
    mapping: std::collections::HashMap<String, String>,
) -> Result<String, String> {
    save_chrome_profile_mapping(&get_runtime_config_file(), &mapping)?;
    log_client_info(&format!("[Chrome映射] 已保存 {} 条映射", mapping.len()));
    Ok(format!("已保存 {} 条映射", mapping.len()))
}

/// 枚举当前所有 chrome.exe 主进程的 `--user-data-dir` 目录名
/// （DEV-125986，独立进程架构映射表确认界面用）。
#[cfg(target_os = "windows")]
fn list_chrome_user_data_dir_names_os() -> Result<Vec<String>, String> {
    let script = build_list_chrome_main_processes_script();
    let output = powershell_no_window(&script)
        .output()
        .map_err(|e| format!("枚举 Chrome 进程失败: {}", e))?;
    let processes = parse_chrome_main_processes_output(&String::from_utf8_lossy(&output.stdout));
    Ok(processes.into_iter().map(|(_, name)| name).collect())
}

#[cfg(not(target_os = "windows"))]
fn list_chrome_user_data_dir_names_os() -> Result<Vec<String>, String> {
    Ok(Vec::new())
}

/// 客户端能下发给插件的指令白名单。
///
/// # 为什么必须是白名单
/// 指令类型直接来自前端。插件侧按 `handlers[cmd.type]` 分发，放开等于让界面
/// 能驱动插件执行任意键名。
///
/// # 与插件侧的对应关系（改这里必须同步改那边）
/// 每一项都要在 `pms-aichat` 的 `background.ts` 里有对应 handler，否则下发过去
/// 会被当未知类型忽略、**永远收不到 ack**，指令就一直堆在队列里、每次心跳都
/// 白传一遍（2026-08-24 移除下发路径时踩的正是这个）。
///
/// | 指令 | 插件侧动作 | 会不会关掉侧边栏 |
/// |---|---|---|
/// | `reconnectWs` | `wsInit()` 重连业务 WS | 否 |
/// | `reload` | `chrome.runtime.reload()` | **会**，且无法由代码重开 |
/// | `refreshSidepanel` | 侧边栏 `location.reload()` | 否 —— 日常该用的「重启」 |
/// | `trigger1688Login` | 清冷却 + 后台打开 1688 登录页 | 否 |
const PLUGIN_COMMANDS: [&str; 4] = [
    "reconnectWs",
    "reload",
    "refreshSidepanel",
    "trigger1688Login",
];

/// 校验前端传来的指令类型（纯函数，可测）
fn validate_plugin_command(kind: &str) -> Result<(), String> {
    if PLUGIN_COMMANDS.contains(&kind) {
        Ok(())
    } else {
        Err(format!("不支持的指令类型: {}", kind))
    }
}

/// 向指定插件实例下发一条指令。
///
/// 指令入队后由**插件下次心跳时取走**（最长等 5 秒），执行成功才回 ack、
/// 客户端据此出队。所以这里返回「已下发」只代表入队成功，**不代表已执行** ——
/// 界面上的措辞要如实反映这一点，否则人点完以为生效了，实际插件可能压根没接。
///
/// # 只做人工触发
/// 本轮不接任何自动判定。两次教训都出在自动触发上：抢焦点（2026-08-21 砍掉
/// 自动拉起侧边栏）、登录死循环（插件侧回滚 9ef8533e）。人点一次是单发，
/// 形不成放大环；要接自动触发得逐条重新评估。
/// 点击 Chrome 工具栏上的插件图标（领导给的方案）。
///
/// 调用 `resources/icon-locator/aichat_icon_locator.py`：截图 + OpenCV
/// 多尺度模板匹配定位图标，然后模拟鼠标点击。脚本原样保留，这里只做薄封装。
///
/// # 目的
/// 侧边栏关掉后无法由代码重新打开（Chrome 强制 `sidePanel.open()` 必须用户
/// 手势），只能靠「点插件图标」这个真实鼠标动作。这是目前唯一可能自动化的路径。
///
/// # ⚠️ 会抢焦点（已知，按指示先走通再评估）
/// 脚本内部 `SetForegroundWindow` 抢前台 + `SetCursorPos`/`mouse_event` 物理
/// 点击。一台机器跑 8~10 个实例时，抢焦点会打断其它实例正在进行的聊天输入
/// ——这是 2026-08-21 停用自动拉起侧边栏的同一条风险。
///
/// 故本命令**只允许人工点击触发**，且界面上必须写清代价。
///
/// # 多实例精确定位（DEV-125986）
/// 传入 `plugin_name` 时，按 [`resolve_lookup_strategy`] 判断该走映射表
/// 还是 UI Automation 定位到目标窗口的 HWND + 屏幕矩形，再传给脚本做
/// `--hwnd`/`--region` 精确点击，不再无差别全屏扫描。
///
/// 两条路径都定位失败时（比如映射表指向的实例 Chrome 尚未启动、或该
/// 机器既没配映射表也没有 Profile 命名），**回退到原有的全屏 + 标题
/// 匹配行为**而不是直接报错——宁可按旧逻辑赌一次（单实例场景下这样
/// 仍然可靠），也不因为新增的精确定位失败就让整个功能不可用。
/// 不传 `plugin_name`（旧调用方式，或前端还没升级）时同样直接走这个
/// 全屏兜底路径。
#[tauri::command]
async fn click_plugin_icon(
    app: tauri::AppHandle,
    plugin_name: Option<String>,
) -> Result<String, String> {
    use tauri::Manager;
    let script = app
        .path()
        .resolve(
            "resources/icon-locator/aichat_icon_locator.py",
            tauri::path::BaseDirectory::Resource,
        )
        .map_err(|e| format!("找不到图标定位脚本: {}", e))?;
    if !script.exists() {
        return Err(format!(
            "图标定位脚本不存在: {:?}。该脚本随安装包分发，开发模式下需确认 \
             src-tauri/resources/icon-locator/ 已就位",
            script
        ));
    }
    let script_str = script.to_string_lossy().to_string();

    let target = match &plugin_name {
        Some(name) => {
            let t = locate_icon_click_target_os(name);
            if t.is_none() {
                log_client_info(&format!(
                    "[图标点击] 未能精确定位实例 {}，回退到全屏扫描",
                    name
                ));
            }
            t
        }
        None => None,
    };

    log_client_info(&format!(
        "[图标点击] 调用脚本: {}（target={:?}）",
        script_str, target
    ));

    let (stdout, stderr, code) = run_icon_locator_os(&script_str, target.as_ref())?;
    match parse_icon_click_result(&stdout, code, &stderr) {
        Ok((x, y)) => {
            log_client_info(&format!("[图标点击] 已点击 ({}, {})", x, y));
            Ok(format!("已点击插件图标，坐标 ({}, {})", x, y))
        }
        Err(e) => {
            log_client_error(&format!("[图标点击] 失败: {}", e));
            Err(e)
        }
    }
}

/// 根据 plugin_name 定位目标窗口的 HWND + 屏幕矩形（DEV-125986）。
///
/// 按 [`resolve_lookup_strategy`] 分流：映射表命中走独立进程架构的
/// 进程枚举链路，未命中走 UI Automation。任一环节失败都返回 `None`
/// 而非报错——调用方据此回退到全屏扫描，不影响单实例场景的可用性。
#[cfg(target_os = "windows")]
fn locate_icon_click_target_os(plugin_name: &str) -> Option<IconClickTarget> {
    let mapping = load_chrome_profile_mapping(&get_runtime_config_file());
    let strategy = resolve_lookup_strategy(plugin_name, &mapping);
    let hwnd = match strategy {
        LookupStrategy::MappingTable(dir_name) => {
            let script = build_list_chrome_main_processes_script();
            let output = powershell_no_window(&script).output().ok()?;
            let processes =
                parse_chrome_main_processes_output(&String::from_utf8_lossy(&output.stdout));
            let pid = find_pid_for_user_data_dir_name(&processes, &dir_name)?;
            let script = build_window_handle_for_pid_script(pid);
            let output = powershell_no_window(&script).output().ok()?;
            parse_window_handle_for_pid_output(&String::from_utf8_lossy(&output.stdout))
        }
        LookupStrategy::UiAutomation => {
            let script = build_ui_automation_probe_script();
            let output = powershell_no_window(&script).output().ok()?;
            let windows =
                parse_ui_automation_probe_output(&String::from_utf8_lossy(&output.stdout));
            find_hwnd_for_plugin_name(&windows, plugin_name)
        }
    }?;

    let script = build_window_rect_script(hwnd);
    let output = powershell_no_window(&script).output().ok()?;
    let region = parse_window_rect_output(&String::from_utf8_lossy(&output.stdout))?;
    Some(IconClickTarget { hwnd, region })
}

/// 非 Windows 平台无法定位（依赖 UI Automation / WMI 等 Win32 专属能力），
/// 直接返回 None，由调用方回退到全屏扫描——本机开发调试用。
#[cfg(not(target_os = "windows"))]
fn locate_icon_click_target_os(_plugin_name: &str) -> Option<IconClickTarget> {
    None
}

/// 执行图标定位脚本，返回 (stdout, stderr, 退出码)。
///
/// Windows 用 `python`（目标机器需装 Python 3.8+ 并配好 PATH，
/// 且装有 cv2/numpy/Pillow 三个依赖库——不是版本硬性要求，见脚本
/// 头部说明；2026-08-29 实测某测试机因未装 Python 触发 9009
/// "命令未找到"错误，属部署前置条件缺失，非脚本逻辑问题）。
/// **不能加 CREATE_NO_WINDOW**——脚本本身就要抢前台
/// 才能截到图，隐藏控制台窗口不改变这一点，反而让人看不到它在跑。
#[cfg(target_os = "windows")]
fn run_icon_locator_os(
    script: &str,
    target: Option<&IconClickTarget>,
) -> Result<(String, String, i32), String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let args = build_icon_click_command(script, target);
    // 直接执行 `python`（走 PATH 解析），参数各自独立传递，不经过 cmd 的
    // 字符串拼接/转义——2026-08-29 真机实测过旧的 `cmd /C "python <一整条
    // 字符串>"` 写法在脚本路径带 `\\?\` 长路径前缀时会被引号规则打乱，
    // 见 build_icon_click_command 文档
    let output = std::process::Command::new("python")
        .args(&args)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| format!("启动 python 失败（目标机器是否装了 Python 且已加入 PATH？）: {}", e))?;
    Ok((
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
        output.status.code().unwrap_or(-1),
    ))
}

/// 非 Windows 平台不支持：脚本全是 ctypes.windll 的 Win32 调用
/// （SetForegroundWindow / SetCursorPos / mouse_event），在 macOS 上
/// 连 import 都过不了。采购机器全是 Windows，本分支仅供本机开发时不报错。
#[cfg(not(target_os = "windows"))]
fn run_icon_locator_os(
    _script: &str,
    _target: Option<&IconClickTarget>,
) -> Result<(String, String, i32), String> {
    Err("图标点击只支持 Windows（脚本依赖 Win32 API），当前系统不可用".to_string())
}

// ─────────────────────────────────────────────────────────────────
// 多实例图标定位：独立进程架构识别（DEV-125986）
//
// 真机实测确认采购机器存在两种 Chrome 架构，需要分别处理：
// ①单进程多 Profile——Profile 已手动命名（如"10-2"），走 UI Automation
//   读控件树即可拿到完整 plugin_name，全自动、不需要本节这套逻辑；
// ②独立进程——各自 --user-data-dir 启动，目录名当初随意命名
//   （如 User4/User5），与 plugin_name 之间没有客观对应关系，
//   三者（快捷方式名/目录名/plugin_name）编号互相对不上、无法自动推导，
//   需要人工做一次性映射确认（界面待实现），本节只负责解析出
//   映射表的 key——即 --user-data-dir 的目录名。
// ─────────────────────────────────────────────────────────────────

/// 从 chrome.exe 进程命令行中提取 `--user-data-dir` 参数指向的目录名（最后一段）。
///
/// 用作独立进程架构下查映射表的 key。目录名而非完整路径是因为映射表要跟人工
/// 在界面上填的 key 保持一致——完整路径太长、不同机器盘符也可能不同，
/// 取最后一段目录名足够唯一且更适合人工核对。
pub fn extract_user_data_dir_name(command_line: &str) -> Option<String> {
    let marker = "--user-data-dir=";
    let start = command_line.find(marker)? + marker.len();
    let rest = &command_line[start..];

    // 参数值可能带引号（路径含空格时 Chrome 会加），也可能不带
    let value = if let Some(stripped) = rest.strip_prefix('"') {
        stripped.split('"').next()?
    } else {
        // 不带引号时，值在下一个空格之前结束
        rest.split(' ').next()?
    };

    let trimmed = value.trim_end_matches(['\\', '/']);
    let name = trimmed.rsplit(['\\', '/']).next()?;
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

/// 判断一条 chrome.exe 命令行是否属于「浏览器主进程」而非子进程。
///
/// Chrome 多进程架构下，渲染进程/GPU 进程/网络进程等子进程命令行都带
/// `--type=`，唯独主进程（Browser Process）没有——这是从
/// `Get-CimInstance Win32_Process` 查到的一批 chrome.exe 里筛出
/// 「每个实例真正的那一个主进程」的判据，子进程的 `--user-data-dir`
/// 即使存在也不代表独立实例，不能拿去建映射。
pub fn is_chrome_main_process_command_line(command_line: &str) -> bool {
    !command_line.contains("--type=")
}

/// 定位某个 plugin_name 对应窗口，该走哪条识别路径。
///
/// 映射表优先：它是人工确认过的、最不含糊的依据（独立进程架构，目录名
/// 与 plugin_name 本无客观关联，映射表就是唯一的真相来源）。未命中时
/// 回退到 UI Automation——多数机器是单进程多 Profile 架构，Profile 名
/// 本就是完整 plugin_name，根本不需要维护映射表。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LookupStrategy {
    /// 命中映射表，携带对应的 --user-data-dir 目录名（下一步用它去匹配
    /// chrome.exe 进程的命令行）
    MappingTable(String),
    /// 未命中映射表，走 UI Automation 遍历窗口控件树
    UiAutomation,
}

pub fn resolve_lookup_strategy(
    plugin_name: &str,
    mapping: &std::collections::HashMap<String, String>,
) -> LookupStrategy {
    // 映射表存的是 目录名 -> plugin_name，调用方给的是 plugin_name，
    // 所以要按 value 反查对应的 key，不能假设调用方会传目录名进来
    match mapping.iter().find(|(_, v)| v.as_str() == plugin_name) {
        Some((dir_name, _)) => LookupStrategy::MappingTable(dir_name.clone()),
        None => LookupStrategy::UiAutomation,
    }
}

/// 构建 UI Automation 探测脚本：遍历所有 Chrome 顶层窗口，从控件树里
/// 读出 (HWND, plugin_name) 对。
///
/// 走 PowerShell + `System.Windows.Automation`（而非 Rust `windows` crate
/// 直接调 Win32 API）：跟项目里图标定位、防火墙规则等其它 Windows 交互
/// 保持同一套模式（Rust 拼脚本字符串 → `std::process::Command` 执行 →
/// 解析 stdout），且这段脚本已在真机（10号机，8个 Profile）验证过能
/// 读到目标数据，不需要再踩 FFI 绑定的坑。
///
/// 只对 Name 匹配 `<数字>-<数字>-<字母数字>` 这个 plugin_name 形状的
/// 控件输出，避免把无关的按钮/菜单项文本也当成候选传回来。
pub fn build_ui_automation_probe_script() -> String {
    r#"
Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
$root = [System.Windows.Automation.AutomationElement]::RootElement
$condition = New-Object System.Windows.Automation.PropertyCondition(
    [System.Windows.Automation.AutomationElement]::ClassNameProperty, "Chrome_WidgetWin_1"
)
$chromeWindows = $root.FindAll([System.Windows.Automation.TreeScope]::Children, $condition)
foreach ($win in $chromeWindows) {
    $hwnd = $win.Current.NativeWindowHandle
    $allCondition = [System.Windows.Automation.Condition]::TrueCondition
    $descendants = $win.FindAll([System.Windows.Automation.TreeScope]::Descendants, $allCondition)
    foreach ($el in $descendants) {
        $name = $el.Current.Name
        if ($name -match '^\d+-\d+-\w+$') {
            Write-Output "$hwnd|$name"
        }
    }
}
"#
    .to_string()
}

/// 解析 [`build_ui_automation_probe_script`] 的输出，得到 (HWND, plugin_name) 列表。
///
/// 容忍畸形行（跳过而非整体失败）：巡检场景下少一条数据远好过整个查询
/// 报错。空输出（该架构未使用 Chrome 多用户 Profile 命名功能，或本次
/// 恰好没有匹配到）返回空列表而非报错——调用方据此判断这条路径走不通，
/// 转而检查映射表。
pub fn parse_ui_automation_probe_output(output: &str) -> Vec<(u64, String)> {
    output
        .lines()
        .filter_map(|line| {
            let (hwnd, name) = line.trim().split_once('|')?;
            let hwnd: u64 = hwnd.trim().parse().ok()?;
            let name = name.trim();
            if name.is_empty() {
                None
            } else {
                Some((hwnd, name.to_string()))
            }
        })
        .collect()
}

/// 在 UI Automation 探测得到的候选列表中，精确匹配目标 plugin_name 对应的 HWND。
///
/// 找不到时返回 `None` 而非猜一个近似值——点错实例比点不到更糟：点错会
/// 打断一个无关实例的正在进行的操作，而点不到只是这次操作没生效。
pub fn find_hwnd_for_plugin_name(windows: &[(u64, String)], plugin_name: &str) -> Option<u64> {
    windows
        .iter()
        .find(|(_, name)| name == plugin_name)
        .map(|(hwnd, _)| *hwnd)
}

/// 构建枚举 chrome.exe 进程（含完整命令行）的 PowerShell 脚本。
///
/// 独立进程架构的识别入口：`Get-Process` 默认不返回命令行，必须用
/// `Get-CimInstance Win32_Process`（WMI）才能读到 `--user-data-dir`。
/// 输出契约每行 `<PID>|<CommandLine>`，交给
/// [`parse_chrome_main_processes_output`] 解析。
pub fn build_list_chrome_main_processes_script() -> String {
    "Get-CimInstance Win32_Process -Filter \"Name='chrome.exe'\" | \
     ForEach-Object { Write-Output \"$($_.ProcessId)|$($_.CommandLine)\" }"
        .to_string()
}

/// 解析 [`build_list_chrome_main_processes_script`] 的输出，得到
/// (PID, user-data-dir 目录名) 列表。
///
/// 只保留：①主进程（[`is_chrome_main_process_command_line`]，排除渲染/
/// GPU/网络等子进程——它们的 `--user-data-dir` 不代表独立实例）；
/// ②确实带了 `--user-data-dir` 参数的记录（没带的对建映射没有意义，
/// 跳过而非报错，容错优先）。
pub fn parse_chrome_main_processes_output(output: &str) -> Vec<(u32, String)> {
    output
        .lines()
        .filter_map(|line| {
            let (pid, command_line) = line.trim().split_once('|')?;
            let pid: u32 = pid.trim().parse().ok()?;
            if !is_chrome_main_process_command_line(command_line) {
                return None;
            }
            let dir_name = extract_user_data_dir_name(command_line)?;
            Some((pid, dir_name))
        })
        .collect()
}

/// 在 [`parse_chrome_main_processes_output`] 的结果中，按 `--user-data-dir`
/// 目录名精确匹配对应的进程 PID。
///
/// 找不到时返回 `None`（比如映射表记录的实例，其 Chrome 当前还没启动），
/// 调用方应据此提示「未找到对应进程」而不是误配到别的实例。
pub fn find_pid_for_user_data_dir_name(processes: &[(u32, String)], dir_name: &str) -> Option<u32> {
    processes
        .iter()
        .find(|(_, name)| name == dir_name)
        .map(|(pid, _)| *pid)
}

/// 构建查询指定 PID 主窗口句柄的 PowerShell 脚本。
///
/// 独立进程架构下拿到目标 chrome.exe 的 PID 后，最后一步是找到它的
/// 可见主窗口——`Get-Process -Id` 本身就带 `MainWindowHandle` 属性，
/// 不需要额外的 `GetWindowThreadProcessId` Win32 调用。
pub fn build_window_handle_for_pid_script(pid: u32) -> String {
    format!(
        "(Get-Process -Id {} -ErrorAction SilentlyContinue).MainWindowHandle",
        pid
    )
}

/// 解析 [`build_window_handle_for_pid_script`] 的输出。
///
/// 空输出（进程不存在/已退出）或句柄为 0（存在但没有可见主窗口，
/// Win32 惯例）都返回 `None`——两种情况对调用方而言都是「找不到可点击
/// 的窗口」，不需要区分。
pub fn parse_window_handle_for_pid_output(output: &str) -> Option<u64> {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return None;
    }
    match trimmed.parse::<u64>() {
        Ok(0) => None,
        Ok(hwnd) => Some(hwnd),
        Err(_) => None,
    }
}

/// 构建查询指定 HWND 窗口矩形的 PowerShell 脚本。
///
/// 用 `GetWindowRect`（而非 `.Bounds` 之类的 .NET 包装）是因为它返回的
/// 是虚拟桌面绝对坐标——跟图标定位脚本 `--region` 参数、以及采购机上
/// 多屏场景的坐标系必须严格一致，用别的 API 容易在多屏/DPI 缩放下对不上。
pub fn build_window_rect_script(hwnd: u64) -> String {
    format!(
        "Add-Type -TypeDefinition 'using System; using System.Runtime.InteropServices; \
         public struct RECT {{ public int Left; public int Top; public int Right; public int Bottom; }} \
         public class Win32 {{ [DllImport(\"user32.dll\")] public static extern bool GetWindowRect(IntPtr hWnd, out RECT rect); }}'; \
         $rect = New-Object RECT; \
         [Win32]::GetWindowRect([IntPtr]{}, [ref]$rect) | Out-Null; \
         Write-Output \"$($rect.Left),$($rect.Top),$($rect.Right),$($rect.Bottom)\"",
        hwnd
    )
}

/// 解析 [`build_window_rect_script`] 的输出，转换成图标定位脚本
/// `--region` 参数要的 (left, top, width, height) 格式。
///
/// `GetWindowRect` 原生返回右下角坐标而非宽高，转换在这里做一次，
/// 调用方不需要关心这个差异。
pub fn parse_window_rect_output(output: &str) -> Option<(i32, i32, i32, i32)> {
    let parts: Vec<&str> = output.trim().split(',').collect();
    if parts.len() != 4 {
        return None;
    }
    let nums: Vec<i32> = parts.iter().filter_map(|p| p.trim().parse().ok()).collect();
    if nums.len() != 4 {
        return None;
    }
    let (left, top, right, bottom) = (nums[0], nums[1], nums[2], nums[3]);
    Some((left, top, right - left, bottom - top))
}

#[tauri::command]
fn send_plugin_command(plugin_name: String, kind: String) -> Result<String, String> {
    validate_plugin_command(&kind)?;
    let reg = HEARTBEATS.get().ok_or("心跳服务未启动")?;
    let mut guard = reg.lock().map_err(|_| "心跳状态表不可用".to_string())?;
    match guard.enqueue_command(&plugin_name, &kind) {
        Some(_) => {
            log_client_info(&format!("[巡检] 已向 {} 下发 {} 指令", plugin_name, kind));
            Ok(format!("已下发 {}，等待插件下次心跳取走（最长 5 秒）", kind))
        }
        // enqueue_command 返回 None 有两种原因，对使用者的含义完全不同：
        // 同类指令已在队列（点重复了，无害）vs 该实例从未上报过心跳（插件没连上，
        // 点了也白点）。这里合并成一句话说不清，故分开判断
        None => {
            if guard.has_plugin(&plugin_name) {
                Ok(format!("{} 指令已在队列中，等待执行", kind))
            } else {
                Err(format!(
                    "{} 从未上报过心跳，可能插件未运行或版本过旧（指令需插件 ≥ 支持心跳的版本）",
                    plugin_name
                ))
            }
        }
    }
}

/// 枚举 Chrome 窗口及最小化状态
#[cfg(target_os = "windows")]
fn list_chrome_windows_os() -> Result<Vec<(u32, bool)>, String> {
    let cmd = build_list_chrome_windows_command_windows();
    // 必须走 powershell_no_window：巡检页每 5 秒刷新一次，弹一次控制台窗口
    // 就抢一次前台焦点——同机 8~10 个实例正在往聊天框打字，会被打断、丢字
    let output = powershell_no_window(&cmd)
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
    // 自愈动作全部停用后，这些「需人工介入」的 ERROR 只是观测记录，却会把
    // 「仅异常」视图淹掉：15 个实例 × 每个 30 分钟冷却期各 2 条 ≈ 90 条，
    // 而真正要看的插件异常就混在里面。按插件+原因做时间窗抑制，
    // 保证「一直失联就一直有记录」但频率降到每小时一条。
    let mut throttle = LogThrottle::new(std::time::Duration::from_secs(3600));
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
                    // 重启没真发生，所以这条只是观测记录 —— 用时间窗抑制，
                    // 否则冷却期一到就再刷一轮，把真正的插件异常挤出视野
                    if throttle.should_log(
                        &format!("restart:{}", plugin),
                        std::time::Instant::now(),
                    ) {
                        log_client_error(&format!(
                            "[自愈] {} 彻底失联（已达重启条件，但重启 Chrome 暂未启用），等待人工介入",
                            plugin
                        ));
                    }
                }
                heartbeat::HealAction::RestartSuppressed(reason) => {
                    if throttle.should_log(
                        &format!("suppress:{}:{}", plugin, reason),
                        std::time::Instant::now(),
                    ) {
                        log_client_error(&format!(
                            "[自愈] {} 彻底失联（{}），等待人工介入",
                            plugin, reason
                        ));
                    }
                }
                // 同一原因已上报过，不重复记录——巡检每 5 秒一轮，
                // 不去重会让一台故障机一天攒出上千条相同日志
                heartbeat::HealAction::RestartSuppressedSilently => {}
            }
        }
    });
}

// 「重启 Chrome 并拉起侧边栏」命令同期移除：它内部同样要调
// run_open_sidepanel_os 抢焦点，且二级自愈从未接线（一台机器多开 15 个实例，
// 重启会把全部实例一起干掉，代价远大于收益，见 DEV-124837）。

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

/// 若命令行是 CLI 查询模式则执行并返回 true，调用方据此跳过 GUI。
///
/// # 为什么必须在 Tauri 初始化之前判断
/// `run()` 里注册了 single-instance 插件：常驻进程已在运行时，新进程会把
/// 参数转发给已有实例然后自己退出——CLI 参数会被当成「唤回窗口」的请求，
/// 查询结果永远不会打印。故这个分支必须**早于** `tauri::Builder` 返回。
pub fn try_run_cli() -> bool {
    let argv: Vec<String> = std::env::args().collect();
    let first = argv.get(1).map(|s| s.as_str());
    if first != Some(cli::QUERY_SUBCOMMAND) {
        // 带了参数但不是已知子命令：**不能静默拉起 GUI**。
        // 采购机上有人误传参数（或调用方用了不支持该子命令的旧版）时，
        // 静默弹出界面既打扰用户、又让调用方一直等一个永不返回的进程。
        // 以 `-` 开头视为想用命令行，给出用法并退出；裸参数留给 Tauri
        // 处理（系统可能传入文件路径等）
        if let Some(arg) = first {
            if arg.starts_with('-') {
                cli::attach_console_if_needed();
                println!("{}", cli::usage_text());
                return true;
            }
        }
        return false;
    }
    // Windows 下 GUI 子系统的进程默认无控制台，不附加则输出写进虚空
    cli::attach_console_if_needed();

    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let req = cli::parse_args(&argv[2..], &today);
    // 带上机器标识：将来多台机器的输出汇总到一处时，没有它无法区分来源
    println!(
        "{}",
        cli::execute_with_machine(req, &get_log_dir(), &get_machine_id())
    );
    true
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // CLI 模式：查完即退，不拉起 GUI（须在 Tauri 初始化前，见 try_run_cli 文档）
    if try_run_cli() {
        return;
    }
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
            // 没有这条日志就无法判断十几台机器里谁卡在旧版没升上来。
            // 同时带上「升级前是哪版」，逐台核实 rollout 时才能区分升级与重启
            let previous_version = load_and_record_version(
                &get_config_file_path_with_dir(&get_app_config_dir()),
                &app_version,
            );
            log_client_info(&build_startup_version_line(
                &app_version,
                previous_version.as_deref(),
            ));
            // 启动环境快照：排查时最常问的几个「它到底装在哪、认哪个更新源」
            // 一次性给全，免得为了确认一个路径又要远程折腾一轮
            let endpoint = extract_updater_endpoint(
                &serde_json::to_value(&app.config().plugins).unwrap_or_default(),
            );
            log_client_info(&format!(
                "[启动] machineId={} 更新源={} 日志目录={:?}",
                get_machine_id(),
                endpoint,
                get_log_dir()
            ));
            // 心跳状态表由 HTTP 服务与自愈巡检共享同一实例：
            // 巡检要读 HTTP 侧写入的 last_seen，也要往队列塞待下发指令
            let heartbeats = std::sync::Arc::new(std::sync::Mutex::new(
                heartbeat::HeartbeatRegistry::new(),
            ));
            // 供巡检看板命令读取同一份状态
            let _ = HEARTBEATS.set(heartbeats.clone());
            // WS 握手令牌：每次启动新生成、不持久化，泄露影响限于单次运行周期
            let ws_token = ws_token::generate_token();
            let _ = WS_TOKEN.set(ws_token.clone());
            match log_server::spawn(sink, &app_version, heartbeats.clone(), ws_token) {
                Ok(port) => {
                    LOG_SERVER_PORT.store(port, std::sync::atomic::Ordering::Relaxed);
                    // 端口起来了才有放行的意义。放在这里而不是安装阶段，是因为
                    // 自动升级时更新器不是管理员、NSIS 钩子不会跑（见函数文档）
                    try_add_firewall_rule(port);
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
            get_auto_update,
            set_autostart,
            open_log_dir,
            get_log_server_port,
            log_self_update_error,
            log_self_update_info,
            log_self_update_check,
            log_self_update_failure,
            log_self_update_progress,
            diagnose_download_url,
            send_plugin_command,
            click_plugin_icon,
            list_log_dates,
            read_log_entries,
            read_log_page,
            get_system_snapshot,
            get_patrol_report,
            get_chrome_profile_candidates,
            save_chrome_profile_mapping_cmd,
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
    // 自动检查更新开关（2026-08-27）。默认关——测图标点击这类功能时，
    // 机器在背后自动升级会重启客户端、打断测试。关掉后仍可点上面的
    // 「立即检查更新」手动升级
    let auto_update_item = CheckMenuItem::with_id(
        app,
        "toggle_auto_update",
        "自动检查更新",
        true,
        load_auto_update_from_file(&get_runtime_config_file()),
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
            &auto_update_item,
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
            "toggle_auto_update" => {
                let config_file = get_runtime_config_file();
                let next = !load_auto_update_from_file(&config_file);
                match save_auto_update_to_file(&config_file, next) {
                    // 写盘成功才改勾选态，失败保持原状——避免显示与实际不符
                    Ok(()) => {
                        if let Some(item) = app.menu().and_then(|m| m.get("toggle_auto_update")) {
                            if let Some(check) = item.as_check_menuitem() {
                                let _ = check.set_checked(next);
                            }
                        }
                        log_client_info(&format!(
                            "[设置] 自动检查更新已{}",
                            if next { "开启" } else { "关闭（仍可手动点「立即检查更新」）" }
                        ));
                        // 通知前端立即生效，不必等重启
                        let _ = app.emit("auto-update-changed", next);
                    }
                    Err(e) => log_client_error(&format!("保存自动更新偏好失败: {}", e)),
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
    use std::time::{Duration, Instant};
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
    fn test_auto_update_defaults_to_disabled() {
        // **默认关**（2026-08-27 用户拍板）。与 autostart 的默认关是同一种保守取向，
        // 但代价不同、必须说清：
        //
        // autostart 默认关只是「不改用户的开机项」，没有后果；
        // 而自动更新默认关意味着**新装的机器永远停在安装时那个版本**——
        // 发了修复版也不会自己升上去，除非有人手动点「立即检查更新」。
        //
        // 2026-08-24 刚经历过一次「自动更新链路坏了 6 天没人发现」（CDN 对
        // octet-stream 返回 500），当时的教训正是「静默不升级最难察觉」。
        // 故这个默认值必须配一个显眼的界面提示，见巡检页顶部的告警条。
        let config_file = PathBuf::from("/tmp/non_existent_autoupdate_98765/config.json");
        assert_eq!(
            load_auto_update_from_file(&config_file),
            false,
            "配置缺失时自动更新必须默认关闭（用户 2026-08-27 拍板）"
        );
    }

    #[test]
    fn test_save_and_load_auto_update() {
        let tmp = TempDir::new().expect("创建临时目录失败");
        let config_file = tmp.path().join("config.json");

        save_auto_update_to_file(&config_file, true).expect("开启自动更新失败");
        assert_eq!(
            load_auto_update_from_file(&config_file),
            true,
            "开启后读不到，会导致托盘勾选状态与实际行为不符"
        );

        save_auto_update_to_file(&config_file, false).expect("关闭自动更新失败");
        assert_eq!(
            load_auto_update_from_file(&config_file),
            false,
            "关不掉的话，测试期间机器仍会在背后升级并重启客户端"
        );
    }

    #[test]
    fn test_auto_update_preserves_other_config() {
        // config.json 里还有安装路径、machine_id、last_version。写这个开关时
        // 抹掉它们的后果：用户自定义安装路径丢失、machine_id 重新生成
        // （等于换了台新机器，历史日志全部对不上）
        let tmp = TempDir::new().expect("创建临时目录失败");
        let config_file = tmp.path().join("config.json");

        save_path_to_config_file(&config_file, "online", "/custom/aichat").expect("保存路径失败");
        save_autostart_to_file(&config_file, true).expect("保存自启失败");
        save_auto_update_to_file(&config_file, false).expect("保存自动更新偏好失败");

        assert_eq!(
            load_saved_path_from_file(&config_file, "online"),
            Some("/custom/aichat".to_string()),
            "安装路径被抹掉了"
        );
        assert_eq!(load_autostart_from_file(&config_file), true, "自启偏好被抹掉了");
        assert_eq!(load_auto_update_from_file(&config_file), false);
    }

    #[test]
    fn test_autostart_defaults_to_disabled_when_config_missing() {
        let config_file = PathBuf::from("/tmp/non_existent_autostart_98765/config.json");
        assert_eq!(
            load_autostart_from_file(&config_file),
            false,
            "配置缺失时开机自启必须默认关闭，否则会在用户未同意的情况下静默修改开机项"
        );
    }

    // ─────────────────────────────────────────────────────────────
    // Chrome Profile 映射表持久化（DEV-125986 第7步）：独立进程架构下
    // 人工确认的「目录名 -> plugin_name」对照，存进本机既有配置文件。
    // ─────────────────────────────────────────────────────────────

    #[test]
    fn test_chrome_profile_mapping_defaults_to_empty_when_config_missing() {
        let config_file = PathBuf::from("/tmp/non_existent_mapping_13579/config.json");
        assert!(
            load_chrome_profile_mapping(&config_file).is_empty(),
            "配置缺失时应返回空表，而不是 panic 或返回上次残留的旧数据"
        );
    }

    #[test]
    fn test_save_and_load_chrome_profile_mapping() {
        let tmp = TempDir::new().expect("创建临时目录失败");
        let config_file = tmp.path().join("config.json");

        let mut mapping = std::collections::HashMap::new();
        mapping.insert("User4".to_string(), "10-4-LS10347".to_string());
        mapping.insert("User5".to_string(), "10-5-LS10348".to_string());

        save_chrome_profile_mapping(&config_file, &mapping).expect("保存映射表失败");
        assert_eq!(
            load_chrome_profile_mapping(&config_file),
            mapping,
            "保存后应能原样读回，否则人工确认过的映射关系会丢失，\
             每次都要重新走一遍确认界面"
        );
    }

    #[test]
    fn test_chrome_profile_mapping_preserves_other_config() {
        // 跟 auto_update/autostart 同一份 config.json，写映射表时不能
        // 抹掉安装路径、machine_id 等其它字段
        let tmp = TempDir::new().expect("创建临时目录失败");
        let config_file = tmp.path().join("config.json");

        save_path_to_config_file(&config_file, "online", "/custom/aichat").expect("保存路径失败");
        save_auto_update_to_file(&config_file, true).expect("保存自动更新偏好失败");

        let mut mapping = std::collections::HashMap::new();
        mapping.insert("User4".to_string(), "10-4-LS10347".to_string());
        save_chrome_profile_mapping(&config_file, &mapping).expect("保存映射表失败");

        assert_eq!(
            load_saved_path_from_file(&config_file, "online"),
            Some("/custom/aichat".to_string()),
            "写映射表不该抹掉安装路径"
        );
        assert_eq!(
            load_auto_update_from_file(&config_file),
            true,
            "写映射表不该抹掉自动更新偏好"
        );
    }

    #[test]
    fn test_save_chrome_profile_mapping_overwrites_previous_entries() {
        // 重新走一遍确认界面（如新增实例）应整体覆盖旧表，不是逐项合并——
        // 否则删除某个已下线实例的映射关系时，旧记录会一直残留
        let tmp = TempDir::new().expect("创建临时目录失败");
        let config_file = tmp.path().join("config.json");

        let mut first = std::collections::HashMap::new();
        first.insert("User4".to_string(), "10-4-LS10347".to_string());
        first.insert("User5".to_string(), "10-5-LS10348".to_string());
        save_chrome_profile_mapping(&config_file, &first).expect("首次保存失败");

        let mut second = std::collections::HashMap::new();
        second.insert("User4".to_string(), "10-4-LS10347".to_string());
        save_chrome_profile_mapping(&config_file, &second).expect("二次保存失败");

        assert_eq!(
            load_chrome_profile_mapping(&config_file),
            second,
            "应整体覆盖为最新一次确认的结果，User5 的旧记录不能残留"
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
    // 机器标识（DEV-125123）
    // ─────────────────────────────────────────────────────────

    #[test]
    fn test_build_machine_id_combines_host_and_random() {
        let id = build_machine_id("WIN-BUYER01", "abcdef1234567890");
        assert_eq!(id, "WIN-BUYER01-abcdef12", "应为 主机名-随机8位");
    }

    #[test]
    fn test_build_machine_id_sanitizes_hostname() {
        // 主机名里的空格/中文/标点会让 ID 在日志行、文件名、URL 里都不好处理
        let id = build_machine_id("采购 机器.01", "abcdef1234567890");
        assert!(
            id.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-'),
            "ID 只应含字母数字与横杠，实际: {}",
            id
        );
    }

    #[test]
    fn test_build_machine_id_collapses_dashes() {
        // 全是非法字符的主机名不该产出 `-----1a2b` 这种 ID
        let id = build_machine_id("中文主机", "abcdef1234567890");
        assert!(!id.contains("--"), "不应出现连续横杠: {}", id);
        assert!(!id.starts_with('-'), "不应以横杠开头: {}", id);
    }

    #[test]
    fn test_build_machine_id_falls_back_when_hostname_unusable() {
        let id = build_machine_id("...", "abcdef1234567890");
        assert!(
            id.starts_with("unknown-"),
            "主机名不可用时应回落 unknown，实际: {}",
            id
        );
    }

    #[test]
    fn test_machine_id_is_stable_across_calls() {
        // ID 的用途是把日志归属到机器，一变就等于换了台新机器、历史数据全对不上
        let dir = tempfile::TempDir::new().expect("临时目录");
        let cfg = dir.path().join("config.json");
        let first = load_or_create_machine_id(&cfg, "host-a");
        let second = load_or_create_machine_id(&cfg, "host-a");
        assert_eq!(first, second, "同一配置文件必须返回同一个 ID");
    }

    #[test]
    fn test_machine_id_preserves_other_config_fields() {
        // 复用 config.json 时不得抹掉已有的安装路径等设置
        let dir = tempfile::TempDir::new().expect("临时目录");
        let cfg = dir.path().join("config.json");
        save_path_to_config_file(&cfg, "online", "/Users/x/aichat").expect("写入路径失败");
        save_autostart_to_file(&cfg, true).expect("写入自启失败");

        load_or_create_machine_id(&cfg, "host-a");

        assert_eq!(
            load_saved_path_from_file(&cfg, "online").as_deref(),
            Some("/Users/x/aichat"),
            "生成机器 ID 不得抹掉已保存的安装路径"
        );
        assert!(load_autostart_from_file(&cfg), "不得抹掉自启偏好");
    }

    #[test]
    fn test_machine_id_written_to_config_file() {
        let dir = tempfile::TempDir::new().expect("临时目录");
        let cfg = dir.path().join("config.json");
        let id = load_or_create_machine_id(&cfg, "host-a");
        let content = fs::read_to_string(&cfg).expect("配置应已落盘");
        assert!(
            content.contains(&id),
            "机器 ID 须持久化，否则重启就变了。文件内容: {}",
            content
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
        let msg = build_startup_version_line("0.2.0", None);
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

    // ─────────────────────────────────────────────────────────────
    // 自更新可观测性（DEV-122551 / DEV-122552）
    //
    // 这批日志是「升级前必须埋好」的：十几台采购机升一次成本很高，
    // 升上去才发现少埋了字段，就得再付一次升级代价。
    // ─────────────────────────────────────────────────────────────

    #[test]
    fn test_throttle_allows_first_occurrence() {
        let mut t = LogThrottle::new(Duration::from_secs(3600));
        let now = Instant::now();
        assert!(t.should_log("x", now), "首次出现必须放行，否则问题第一次发生就没记录");
    }

    #[test]
    fn test_throttle_suppresses_within_window() {
        let mut t = LogThrottle::new(Duration::from_secs(3600));
        let now = Instant::now();
        assert!(t.should_log("x", now));
        assert!(
            !t.should_log("x", now + Duration::from_secs(60)),
            "窗口内重复应抑制，否则每 5 秒一轮的巡检会把异常视图刷满"
        );
    }

    #[test]
    fn test_throttle_allows_again_after_window() {
        // 这是本结构存在的核心理由：旧写法是「同一错误只记一次，直到成功才解除」，
        // 而持续失败（比如断网）时永远等不到成功 —— 第一条之后永久静默，
        // 排查者看到没有日志会误判成「这台机器没问题」。
        let mut t = LogThrottle::new(Duration::from_secs(3600));
        let now = Instant::now();
        assert!(t.should_log("x", now));
        assert!(
            t.should_log("x", now + Duration::from_secs(3601)),
            "窗口过后必须再放行：一直坏就要一直有记录，只是频率被压下来"
        );
    }

    #[test]
    fn test_throttle_keys_are_independent() {
        let mut t = LogThrottle::new(Duration::from_secs(3600));
        let now = Instant::now();
        assert!(t.should_log("a", now));
        assert!(
            t.should_log("b", now),
            "不同 key 互不影响，否则一个插件的故障会掩盖另一个的"
        );
    }

    #[test]
    fn test_check_line_records_both_versions_and_verdict() {
        // 成功时也要记 —— 现在成功路径什么都不写，导致「这台机器还在检查更新吗」
        // 根本判断不了：定时器死了和一切正常在日志里长得一模一样。
        // 有了这条，4 小时一次的节律本身就是心跳。
        let line = build_self_update_check_line("auto", "0.3.1", "0.3.1", "已是最新", 412);
        assert!(line.contains("当前=0.3.1"), "必须带当前版本，否则不知道这台跑的是哪版");
        assert!(line.contains("远端=0.3.1"), "必须带远端版本，否则不知道它在跟什么比");
        assert!(line.contains("已是最新"));
        assert!(line.contains("412ms"), "耗时用于识别「能连上但很慢」");
        assert!(
            line.contains("[自更新]"),
            "统一前缀，便于用 --keyword 一次筛出整条自更新链路"
        );
        assert!(
            line.contains("触发=auto"),
            "必须区分定时轮询与手动点击：这条当心跳用，2026-08-24 真机上一次连点\
             就刷了 10 条一模一样的，混在一起会让节律判断失效"
        );
    }

    #[test]
    fn test_command_output_never_leaves_empty_detail() {
        // 2026-08-24 真机实测：netsh 失败时日志冒号后面是空的——它把说明写到
        // stdout 而非 stderr。这条日志是防火墙盲区的唯一仪表，空白等于没仪表
        let both_empty = describe_command_output(Some(1), "", "");
        assert!(both_empty.contains("无输出"), "全空时要明说，而不是留一段让人以为被截断的空白");
        assert!(both_empty.contains("退出码=1"), "退出码本身就是信息，必须带上");

        let only_stdout = describe_command_output(Some(1), "拒绝访问。", "");
        assert!(
            only_stdout.contains("拒绝访问"),
            "只取 stderr 就会丢掉 netsh 写在 stdout 里的真正原因"
        );

        let killed = describe_command_output(None, "", "");
        assert!(killed.contains("未知"), "被信号终止时也要给出可读说明，不能是空的");
    }

    #[test]
    fn test_failure_line_carries_url_and_detail() {
        // 2026-08-24 那个 500 只记了一句 message，定位根因靠反复 curl 才做到。
        // 把地址和详情落进日志，下次一眼就完。
        let line = build_self_update_failure_line(
            "下载",
            "https://x/a%20b.exe",
            "状态=500 响应体=<html>...nginx",
            287,
        );
        assert!(line.contains("下载失败"));
        assert!(line.contains("https://x/a%20b.exe"), "必须带下载地址：指错环境时靠它发现");
        assert!(line.contains("状态=500"));
        assert!(line.contains("287ms"));
    }

    #[test]
    fn test_progress_line_shows_human_readable_size_and_speed() {
        // 现在只在下载完成后记一次总量，所以「卡住」（不是失败、就是慢）看不出来
        let line = build_download_progress_line(1_100_000, 4_341_507, 400);
        assert!(line.contains("25%"), "百分比让人一眼看出卡在哪个阶段");
        assert!(line.contains("MB"), "字节数要人类可读，采购机日志也可能被人直接看");
        assert!(line.contains("/s"), "速度用于区分「断了」和「很慢」");
    }

    #[test]
    fn test_progress_line_handles_unknown_total() {
        // Content-Length 缺失时 total 为 0，不能除零 panic
        let line = build_download_progress_line(1024, 0, 100);
        assert!(
            !line.contains("%"),
            "总量未知时不应给出百分比，否则显示一个凭空算出来的假进度"
        );
    }

    #[test]
    fn test_startup_line_distinguishes_upgrade_from_restart() {
        // 这是验证 rollout 的关键一条：要能区分「这台完成了升级」和「这台只是重启了」。
        // 否则十几台机器逐台核实时，看到「启动，版本 0.3.1」无法判断它是刚升上来的
        // 还是本来就是 0.3.1。
        let upgraded = build_startup_version_line("0.3.1", Some("0.3.0"));
        assert!(
            upgraded.contains("0.3.0"),
            "版本变化时必须带上一版本，这是判断升级成功与否的唯一依据"
        );

        let restarted = build_startup_version_line("0.3.1", Some("0.3.1"));
        assert!(
            !restarted.contains("升级前"),
            "版本没变说明只是重启，不该报成升级，否则核实 rollout 时全是假阳性"
        );

        let first_run = build_startup_version_line("0.3.1", None);
        assert!(
            first_run.contains("0.3.1"),
            "首次安装无历史记录时仍要记版本"
        );
        assert!(
            upgraded.contains("客户端启动") && first_run.contains("客户端启动"),
            "前缀保持不变，既有按关键词筛版本分布的用法不能被破坏"
        );
    }

    #[test]
    fn test_record_version_returns_previous_and_persists_current() {
        let tmp = tempfile::tempdir().expect("创建临时目录");
        let cfg = tmp.path().join("config.json");

        assert_eq!(
            load_and_record_version(&cfg, "0.3.0"),
            None,
            "首次调用没有历史版本，应返回 None 而非空串（空串会被误判成「上一版本是空」）"
        );
        assert_eq!(
            load_and_record_version(&cfg, "0.3.1"),
            Some("0.3.0".to_string()),
            "第二次调用应拿到上次记录的版本，这样启动日志才能说出「升级前是哪版」"
        );
    }

    #[test]
    fn test_record_version_preserves_other_config_fields() {
        // config.json 里还有安装路径与 machine_id。写版本号时抹掉它们，
        // 后果是用户自定义的安装路径丢失、machine_id 重新生成（等于换了台新机器，
        // 历史日志全部对不上）。
        let tmp = tempfile::tempdir().expect("创建临时目录");
        let cfg = tmp.path().join("config.json");
        fs::write(
            &cfg,
            r#"{"online_path":"D:\\aichat","machine_id":"WIN-A-1234abcd","别的程序写的":"保留"}"#,
        )
        .expect("写入初始配置");

        load_and_record_version(&cfg, "0.3.1");

        let text = fs::read_to_string(&cfg).expect("读回配置");
        assert!(text.contains("D:"), "自定义安装路径必须保留");
        assert!(text.contains("WIN-A-1234abcd"), "machine_id 必须保留");
        assert!(text.contains("别的程序写的"), "未声明字段必须透传（serde flatten 的作用）");
    }

    #[test]
    fn test_diagnosis_identifies_accept_header_rejection() {
        // 这是 2026-08-24 那个根因的机器可读版本：同一地址，带 Accept 500、不带 200。
        // 埋这条的价值是让下一次同类问题在日志里自己说出结论，不用人去 curl 对比。
        let v = build_diagnosis_verdict(Some(500), Some(200));
        assert!(
            v.contains("Accept"),
            "带头失败、不带头成功 ⇒ 必须指向请求头，而不是让人误以为是网络故障"
        );
    }

    #[test]
    fn test_diagnosis_identifies_network_failure() {
        let v = build_diagnosis_verdict(None, None);
        assert!(
            v.contains("网络") || v.contains("DNS"),
            "两种请求都发不出去 ⇒ 指向网络/DNS，不该误报成服务端问题"
        );
    }

    #[test]
    fn test_diagnosis_reports_transient_when_probe_succeeds() {
        // 下载失败了，但诊断时同样的请求又成功了 —— 这种要明确说成瞬时，
        // 否则排查者会因为「我手动试是好的」而怀疑日志在骗人
        let v = build_diagnosis_verdict(Some(200), Some(200));
        assert!(v.contains("瞬时"), "诊断时正常应明确报瞬时故障，避免与手动复现结果矛盾");
    }

    /// 对**真实服务端**验证探针能得出正确结论。默认不跑（要联网）：
    /// `cargo test -- --ignored probe_real_server`
    ///
    /// 存在理由：`build_diagnosis_verdict` 的单测只证明「给定两个状态码时结论对」，
    /// 不证明「服务端真的会这么回」。而这次故障的全部要害就在服务端的实际行为上，
    /// 那部分只有打一次真实请求才算证实。这条同时充当**回归哨兵** ——
    /// 运维把 nginx 修好之后它会失败，正好提示可以把下载地址切回自建站。
    #[test]
    #[ignore = "需要联网访问 chainai 站点"]
    fn test_probe_real_server_detects_accept_rejection() {
        let rt = tokio::runtime::Runtime::new().expect("创建 tokio runtime");
        let (with_accept, without_accept) = rt
            .block_on(probe_download_url(
                "https://chainai.cjdropshipping.cn/updater/latest.json",
            ))
            .expect("探针应能创建 HTTP 客户端");
        let verdict = build_diagnosis_verdict(with_accept, without_accept);
        println!("带 Accept={:?} 不带={:?} ⇒ {}", with_accept, without_accept, verdict);
        assert_eq!(
            with_accept,
            Some(500),
            "2026-08-24 实测：该站点对 Accept: application/octet-stream 整站返回 500。\
             若此断言失败，说明服务端已修好，可以把 latest.json 的下载地址切回自建站"
        );
        assert_eq!(
            without_accept,
            Some(200),
            "同一地址不带 Accept 应返回 200——这个对比正是根因判定的依据"
        );
    }

    // ─────────────────────────────────────────────────────────────
    // 点插件图标（领导给的方案：模板匹配定位 + 点击）
    //
    // 背景：领导发来 docs/chatgpt-icon-locator，用 OpenCV 多尺度模板匹配
    // 在 Chrome 工具栏上找扩展图标并点击。这里只做**薄封装**——不篡改
    // 脚本逻辑，只把参数拼好、跑起来、按契约解析结果。
    //
    // ⚠️ 已知问题但按用户指示先走通：脚本会 SetForegroundWindow 抢前台
    // + SetCursorPos/mouse_event 物理点击。这在单窗口 RPA 场景没问题，
    // 但我们是 8~10 实例并行，抢焦点会打断正在输入的实例。此为领导
    // 方案的既有属性，接入时如实保留，走通后再评估冲突。
    // ─────────────────────────────────────────────────────────────

    #[test]
    fn test_build_icon_click_command_include_activate_and_click() {
        // 理想的一组参数（README 里写的）：激活 Chrome + 找到图标 + 直接点。
        // 2026-08-29 真机实测暴露的 bug：旧实现返回一整条字符串交给
        // `cmd /C "python <script> ..."` 拼接执行，脚本路径若含
        // Tauri 在某些场景下返回的 `\\?\` 长路径前缀，会被 cmd 的引号/
        // 转义规则打乱，实际执行时报 "can't open file" 且路径被拼成
        // 相对于 C:\Windows\system32 的畸形路径。
        // 改为返回参数数组（Vec<String>），交给 `Command::new("python").args(..)`
        // 直接执行——不经过任何 shell 字符串解析，从根上避免这类转义问题。
        let args = build_icon_click_command("/opt/icon-locator/aichat_icon_locator.py", None);
        assert_eq!(
            args[0], "/opt/icon-locator/aichat_icon_locator.py",
            "脚本路径必须是独立的一个参数，不能和其它参数拼在同一个字符串里"
        );
        assert!(args.contains(&"--activate".to_string()), "必须先激活 Chrome——图标可能被其它窗口遮挡");
        assert!(args.contains(&"--click".to_string()), "找到后要直接点，别把坐标传回来再转一手");
        assert!(args.contains(&"--activate-match".to_string()), "要指定挑哪个 Chrome 窗口");
        assert!(args.contains(&"Google Chrome".to_string()), "标题匹配串本身也要是独立参数，不能被外层引号吞掉空格");
    }

    #[test]
    fn test_build_icon_click_command_with_target_uses_hwnd_and_region() {
        // DEV-125986：给定精确定位到的目标（HWND + 窗口矩形）时，
        // 命令应该用 --hwnd 精确激活、用 --region 限定匹配范围，
        // 而不是继续用无差别的 --activate-match "Google Chrome"
        let target = IconClickTarget {
            hwnd: 197304,
            region: (100, 50, 300, 80),
        };
        let args = build_icon_click_command("/opt/icon-locator/aichat_icon_locator.py", Some(&target));
        assert!(args.contains(&"--hwnd".to_string()), "有明确目标时必须精确指定 HWND，不能再靠标题匹配猜");
        assert!(args.contains(&"197304".to_string()));
        assert!(
            args.contains(&"--region".to_string()) && args.contains(&"100,50,300,80".to_string()),
            "必须把匹配范围收窄到目标窗口，不能继续全屏扫描"
        );
        assert!(args.contains(&"--activate".to_string()), "仍需要先激活确保窗口可见可点");
        assert!(args.contains(&"--click".to_string()), "仍需要直接点击");
        assert!(
            !args.contains(&"--activate-match".to_string()),
            "有精确 HWND 时不应再传标题匹配参数，避免脚本里两套定位逻辑混用产生歧义"
        );
    }

    #[test]
    fn test_icon_locator_resources_exist_and_agree() {
        // 三处必须一致：Rust 侧引用的脚本名、脚本里的默认模板名、实际文件。
        // 不一致的表现是运行时才报「模板缺失」，而那时人已经在虚拟机前了。
        //
        // 领导给的原始脚本默认模板是 chatgpt_template.png（他自己的场景），
        // 拿它去找我们的蓝色文档+放大镜图标必然 NOT_FOUND——换模板时若漏改
        // 脚本里那行常量，症状就是「点了按钮永远说找不到」，很难联想到根因。
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("resources/icon-locator");
        let script = dir.join("aichat_icon_locator.py");
        let template = dir.join("aichat_template.png");
        assert!(script.exists(), "脚本不存在: {:?}", script);
        assert!(template.exists(), "模板图不存在: {:?}", template);

        let src = fs::read_to_string(&script).expect("读取脚本");
        assert!(
            src.contains("aichat_template.png"),
            "脚本里的 DEFAULT_TEMPLATE 没指向 aichat_template.png——\
             会去找一个不存在的文件，运行时才报错"
        );
    }

    #[test]
    fn test_icon_locator_script_supports_region_argument() {
        // DEV-125986 第5步：多实例场景下，客户端已经能定位到目标 HWND 的
        // 屏幕矩形，脚本必须支持「只在这个矩形内找图标」而不是永远全屏
        // 扫描——否则前面几步的精确定位全部白做，点击时依然可能撞上别的
        // 实例的同款图标。
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("resources/icon-locator");
        let script = dir.join("aichat_icon_locator.py");
        let src = fs::read_to_string(&script).expect("读取脚本");
        assert!(
            src.contains("--region"),
            "脚本必须新增 --region 参数（left,top,width,height），\
             用于把模板匹配范围收窄到目标窗口内，不再无差别扫全屏"
        );
        assert!(
            src.contains("not args.region") || src.contains("args.region is None") || src.contains("if args.region:"),
            "不传 --region 时必须保持原有全屏扫描行为（向后兼容），\
             不能让这个新参数变成强制项"
        );
    }

    #[test]
    fn test_parse_icon_click_result_success() {
        // 脚本契约：stdout 一行 x,y（物理像素绝对坐标），退出码 0
        let r = parse_icon_click_result("1678,62", 0, "");
        assert_eq!(r, Ok((1678, 62)), "标准输出是一行逗号分隔的坐标，解析后返回 (x,y)");
    }

    #[test]
    fn test_parse_icon_click_result_not_found() {
        // 退出码 2：置信度低于阈值，没找到。stdout 为空，错误在 stderr
        let r = parse_icon_click_result("", 2, "NOT_FOUND confidence=0.42 (<0.55)");
        assert!(r.is_err(), "退出码 2 = 没找到，这不是成功");
        if let Err(e) = r {
            assert!(e.contains("未找到"), "要明确指出是「没找到」而不是笼统的失败");
        }
    }

    #[test]
    fn test_parse_icon_click_result_script_error() {
        // 退出码 1：脚本本身出错（模板缺失、没找到 Chrome 窗口等）
        let r = parse_icon_click_result("", 1, "ERROR: template not found");
        assert!(r.is_err());
    }

    #[test]
    fn test_parse_icon_click_result_garbled_stdout() {
        // stdout 不是 x,y 形式（脚本被别的进程污染、或平台差异）——
        // 应该报错，而不是静默返回 (0,0) 去点屏幕左上角
        let r = parse_icon_click_result("hello world", 0, "");
        assert!(r.is_err(), "无法解析成坐标时宁可失败，也不能乱点");
    }

    #[test]
    fn test_parse_icon_click_result_command_not_found() {
        // 2026-08-29 真机实测踩过：目标机器未装 Python/未加入 PATH 时，
        // cmd 内部执行 "python <脚本>" 会返回 Windows 标准错误码 9009
        // （"不是内部或外部命令"）。原文案只把这个数字甩给用户，非技术
        // 采购同事完全看不懂——必须点出最常见的根因，而不是让人去查
        // Windows 错误码含义
        let r = parse_icon_click_result("", 9009, "");
        assert!(r.is_err());
        if let Err(e) = r {
            assert!(
                e.contains("Python") && (e.contains("安装") || e.contains("PATH")),
                "9009 是 Windows 标准的「命令未找到」错误码，报错必须直接点出\
                 最常见根因（未装 Python / 未加入 PATH），而不是只报一个数字。实际报错: {}",
                e
            );
        }
    }

    #[test]
    fn test_extract_updater_endpoint_reads_first_configured_url() {
        // 从真实配置里取而不是另抄一份常量：抄的那份迟早和 tauri.conf.json 不一致，
        // 而「日志说的地址」与「实际请求的地址」不同会把排查带偏很久
        let plugins = serde_json::json!({
            "updater": { "endpoints": ["https://x/updater/latest.json"] }
        });
        assert_eq!(
            extract_updater_endpoint(&plugins),
            "https://x/updater/latest.json"
        );
    }

    #[test]
    fn test_extract_updater_endpoint_tolerates_missing_config() {
        // 读不到配置只是日志里少一个字段，不该 panic 拖垮启动
        assert_eq!(
            extract_updater_endpoint(&serde_json::json!({})),
            "未配置"
        );
        assert_eq!(
            extract_updater_endpoint(&serde_json::json!({"updater": {}})),
            "未配置"
        );
    }

    #[test]
    fn test_plugin_command_whitelist_rejects_unknown_kind() {
        // 指令类型直接来自前端，必须白名单。放开等于让界面能让插件执行任意
        // 字符串指令——插件侧是按 handlers[cmd.type] 分发的，将来新增 handler
        // 时白名单没跟上还好（只是用不了），反过来白名单先放开就是隐患
        assert!(validate_plugin_command("reconnectWs").is_ok());
        assert!(validate_plugin_command("reload").is_ok());
        assert!(validate_plugin_command("refreshSidepanel").is_ok());
        assert!(validate_plugin_command("trigger1688Login").is_ok());

        assert!(validate_plugin_command("rm -rf").is_err());
        assert!(validate_plugin_command("").is_err());
        // 大小写不同即不同指令：插件侧是精确匹配 handlers 的键
        assert!(validate_plugin_command("Reload").is_err());
    }

    #[test]
    fn test_plugin_command_whitelist_matches_plugin_handlers() {
        // 客户端白名单与插件侧 handlers 必须一一对应。少了 → 界面上的按钮
        // 点了没反应；多了 → 下发一条插件不认识的指令，被当未知类型忽略、
        // 永远收不到 ack，于是在队列里一直堆着、每次心跳都白传一遍
        // （2026-08-24 移除下发路径时踩的正是这个）。
        //
        // 插件侧代码不在本仓库，无法编译期校验，故把清单显式列在这里，
        // 改动任一边都要同步改这个断言。
        let expected = ["reconnectWs", "reload", "refreshSidepanel", "trigger1688Login"];
        assert_eq!(
            PLUGIN_COMMANDS, expected,
            "客户端白名单变了：请同步确认 pms-aichat 的 background.ts handlers 里\
             有对应实现，否则指令会永远堆在队列里"
        );
    }

    #[test]
    fn test_no_bare_powershell_invocation_outside_helper() {
        // 少加 CREATE_NO_WINDOW 的后果不是「界面难看」，是**抢走前台焦点**。
        // 2026-08-21 因为抢焦点砍掉了「自动拉起侧边栏」整条路径；2026-08-25 在
        // 真机上发现巡检页仍每 5 秒弹一次 PowerShell 窗口——list_chrome_windows_os
        // 漏了这个标志。同一个坑从另一个地方漏了进来，而这种遗漏肉眼扫不出来。
        //
        // 故用测试挡住：除 powershell_no_window 自身外，不允许再出现裸调用。
        let src = include_str!("lib.rs");
        // 拼接而非写成字面量：直接写全会让本测试自身也被计入，
        // 断言数就永远比实际多 1（第一版就是这么挂的）
        let needle = format!("Command::new({}powershell{})", '"', '"');
        let bare = src.matches(needle.as_str()).count();
        assert_eq!(
            bare, 1,
            "只允许 powershell_no_window 内部有一处裸调用；\
             新增 PowerShell 调用请走该辅助函数，否则会弹控制台窗口抢焦点，\
             打断同机其它实例往供应商聊天框的输入"
        );
    }

    #[test]
    fn test_firewall_powershell_script_converts_output_encoding() {
        // 2026-08-24 真机（中文 Windows）实测这条日志出来是
        // 「stdout=����Ĳ�����Ҫ����」——netsh 按 GBK 输出、Rust 按 UTF-8 解。
        // 靠猜能还原成「请求的操作需要提升」，但下次遇到没见过的错误就读不出来了
        let s = build_firewall_powershell_script(17653);
        assert!(
            s.contains("[Console]::OutputEncoding = [System.Text.Encoding]::UTF8"),
            "必须把 PowerShell 的输出编码切成 UTF-8，否则中文 Windows 上拿到的是乱码"
        );
        let enc_pos = s.find("OutputEncoding").expect("应含编码设置");
        let out_pos = s.find("$out = ").expect("应先捕获 netsh 输出");
        assert!(
            out_pos < enc_pos,
            "必须先用默认(OEM)编码读 netsh 输出、再切 UTF-8 写出。顺序反了会用 UTF-8 去解 GBK 字节，比不转还糟"
        );
        let code_pos = s.find("$code = $LASTEXITCODE").expect("应保存退出码");
        assert!(
            code_pos < enc_pos,
            "退出码要在切编码之前取——中间任何一条命令都会覆盖 $LASTEXITCODE"
        );
        assert!(s.contains("delete rule"), "先删后加保持幂等，否则重复安装会堆出多条同名规则");
    }

    #[test]
    fn test_firewall_script_targets_given_port_only() {
        let script = build_firewall_rule_script_windows(17653);
        assert!(script.contains("17653"), "必须放行实际使用的端口");
        assert!(
            script.contains("dir=in"),
            "只放行入站：出站本来就不受限，多加规则等于扩大暴露面"
        );
        assert!(
            !script.contains("dir=out"),
            "不应放行出站，避免超出「让 AI 读日志」这一必要范围"
        );
    }

    // ─────────────────────────────────────────────────────────────
    // 多实例图标定位：独立进程架构识别（DEV-125986）
    //
    // 背景：真机实测确认采购机器存在两种 Chrome 架构——
    // ①单进程多 Profile（Profile 已手动命名如"10-2"，走 UI Automation
    //   读控件树即可拿到完整 plugin_name，见另一组测试）；
    // ②独立进程（各自 --user-data-dir 启动，目录名当初随意命名如
    //   User4/User5，与 plugin_name 无客观对应关系，无法自动推导）。
    //
    // 本组测试覆盖②：从 chrome.exe 进程命令行里解析出 --user-data-dir
    // 的目录名，作为后续查映射表的 key。
    // ─────────────────────────────────────────────────────────────

    #[test]
    fn test_extract_user_data_dir_name_from_command_line() {
        let cmd = r#""C:\Program Files\Google\Chrome\Application\chrome.exe" --user-data-dir="C:\MyChromeData\User4" --flag-switches-begin"#;
        assert_eq!(
            extract_user_data_dir_name(cmd),
            Some("User4".to_string()),
            "应取 --user-data-dir 路径的最后一段目录名作为映射表的 key"
        );
    }

    #[test]
    fn test_extract_user_data_dir_name_trailing_slash() {
        // 路径末尾带斜杠时，最后一段不能读成空字符串
        let cmd = r#""chrome.exe" --user-data-dir="C:\MyChromeData\User5\" --other-flag"#;
        assert_eq!(
            extract_user_data_dir_name(cmd),
            Some("User5".to_string()),
            "末尾斜杠应被忽略，不能取到空字符串"
        );
    }

    #[test]
    fn test_extract_user_data_dir_name_missing_returns_none() {
        // 子进程（渲染进程等）命令行里没有这个参数，必须返回 None 而不是 panic
        let cmd = r#""chrome.exe" --type=renderer --extension-process --lang=zh-CN"#;
        assert_eq!(
            extract_user_data_dir_name(cmd),
            None,
            "没有 --user-data-dir 参数时应返回 None，不能误判成某个无关字段"
        );
    }

    #[test]
    fn test_extract_user_data_dir_name_unquoted_value() {
        // 命令行里该参数值不一定带引号（无空格路径时 Chrome 不会加引号）
        let cmd = r#""chrome.exe" --user-data-dir=C:\MyChromeData\User6 --flag-switches-end"#;
        assert_eq!(
            extract_user_data_dir_name(cmd),
            Some("User6".to_string()),
            "不带引号的参数值也要能解析"
        );
    }

    #[test]
    fn test_is_chrome_main_process_command_line() {
        // 主进程（浏览器进程）命令行不带 --type=，子进程都带。
        // 用于从 Get-CimInstance 查到的一批 chrome.exe 里筛出主进程
        let main = r#""chrome.exe" --user-data-dir="C:\MyChromeData\User4" --flag-switches-begin --flag-switches-end"#;
        let renderer = r#""chrome.exe" --type=renderer --extension-process --lang=zh-CN"#;
        assert!(is_chrome_main_process_command_line(main), "不带 --type= 的是主进程");
        assert!(
            !is_chrome_main_process_command_line(renderer),
            "带 --type= 的是子进程（渲染/GPU/网络等），不是要找的浏览器主进程"
        );
    }

    // ─────────────────────────────────────────────────────────────
    // 定位策略路由（DEV-125986）：给定 plugin_name，判断该走映射表
    // 还是 UI Automation。纯决策逻辑，不碰任何 Windows API，是两条
    // 识别路径的调度入口。
    // ─────────────────────────────────────────────────────────────

    #[test]
    fn test_resolve_lookup_strategy_uses_mapping_table_when_present() {
        let mut map = std::collections::HashMap::new();
        map.insert("User4".to_string(), "10-4-LS10347".to_string());
        assert_eq!(
            resolve_lookup_strategy("10-4-LS10347", &map),
            LookupStrategy::MappingTable("User4".to_string()),
            "映射表里有该 plugin_name 的记录时，优先按映射表查（独立进程架构，最确定的路径）"
        );
    }

    #[test]
    fn test_resolve_lookup_strategy_falls_back_to_ui_automation() {
        // 映射表里没有这个 plugin_name 时，尝试 UI Automation
        // （对应单进程多 Profile 架构，Profile 名本就是完整 plugin_name，
        // 不需要映射表）
        let map = std::collections::HashMap::new();
        assert_eq!(
            resolve_lookup_strategy("10-2-LS10345", &map),
            LookupStrategy::UiAutomation,
            "映射表未命中时应回退到 UI Automation，而不是直接判定失败——\
             很多机器（单进程多 Profile 架构）根本不需要映射表"
        );
    }

    #[test]
    fn test_resolve_lookup_strategy_mapping_table_lookup_is_by_plugin_name_not_key() {
        // 映射表的 key 是目录名、value 才是 plugin_name——调用方传入的是
        // plugin_name，函数内部要反向查找，而不是直接拿 plugin_name 当 key 查
        let mut map = std::collections::HashMap::new();
        map.insert("User4".to_string(), "10-4-LS10347".to_string());
        map.insert("User5".to_string(), "10-5-LS10348".to_string());
        assert_eq!(
            resolve_lookup_strategy("10-5-LS10348", &map),
            LookupStrategy::MappingTable("User5".to_string()),
            "必须按 value（plugin_name）反查对应的 key（目录名），\
             不能假设调用方会传目录名进来"
        );
    }

    // ─────────────────────────────────────────────────────────────
    // UI Automation 遍历（DEV-125986）：单进程多 Profile 架构下，
    // 从 Chrome 窗口控件树里读出完整 plugin_name。
    //
    // 2026-08-28 真机验证（10号机，8个已命名 Profile）：遍历所有
    // Chrome_WidgetWin_1 顶层窗口，对每个窗口做 UI Automation 子孙
    // 遍历，能找到形如 [ControlType.Text] Name="10-2-LS10345" 的控件，
    // Name 就是完整 plugin_name，不需要额外解析。
    //
    // 走 PowerShell 脚本模式（跟图标定位/防火墙规则等现有 Windows 交互
    // 一致），不引入 windows crate：Rust 侧只负责拼脚本字符串和解析
    // stdout，脚本本身就是真机验证过的那段。
    // ─────────────────────────────────────────────────────────────

    #[test]
    fn test_build_ui_automation_probe_script_contains_verified_logic() {
        let script = build_ui_automation_probe_script();
        assert!(
            script.contains("Chrome_WidgetWin_1"),
            "必须按这个类名筛选 Chrome 顶层窗口，这是真机验证过的窗口类名"
        );
        assert!(
            script.contains("UIAutomationClient"),
            "必须加载 UI Automation 客户端程序集才能用 AutomationElement"
        );
        assert!(
            script.contains("TreeScope]::Descendants"),
            "必须做子孙遍历（不能只看直接子节点），验证过目标控件在较深层级"
        );
        assert!(
            script.contains("NativeWindowHandle"),
            "必须带出每个窗口的 HWND，否则读到 plugin_name 也定位不到窗口"
        );
    }

    #[test]
    fn test_parse_ui_automation_probe_output_single_window() {
        // 脚本输出契约：每行 "<HWND>|<Name>"，只保留看起来像 plugin_name
        // 的行（脚本内部已用正则过滤，这里的解析函数只管拆分）
        let output = "197304|10-2-LS10345\n262908|10-1-LS10344\n";
        let result = parse_ui_automation_probe_output(output);
        assert_eq!(
            result,
            vec![
                (197304u64, "10-2-LS10345".to_string()),
                (262908u64, "10-1-LS10344".to_string()),
            ],
            "应按行拆出 (HWND, plugin_name) 对，顺序与输出一致"
        );
    }

    #[test]
    fn test_parse_ui_automation_probe_output_empty() {
        // 单进程多 Profile 架构但 Profile 未命名时，脚本会输出空——
        // 不能 panic，应返回空列表，让调用方据此判断走不通这条路径
        assert_eq!(
            parse_ui_automation_probe_output(""),
            Vec::<(u64, String)>::new(),
            "空输出应返回空列表，不能 panic 或误判成有数据"
        );
    }

    #[test]
    fn test_parse_ui_automation_probe_output_skips_malformed_lines() {
        // 容错：某一行格式异常时跳过而非整体失败——巡检场景下
        // 少一条数据远好过整个查询报错
        let output = "197304|10-2-LS10345\n畸形行没有分隔符\n262908|10-1-LS10344\n";
        let result = parse_ui_automation_probe_output(output);
        assert_eq!(
            result,
            vec![
                (197304u64, "10-2-LS10345".to_string()),
                (262908u64, "10-1-LS10344".to_string()),
            ],
            "畸形行应被跳过，不影响其它正常行的解析"
        );
    }

    #[test]
    fn test_find_hwnd_for_plugin_name_matches() {
        let windows = vec![
            (197304u64, "10-2-LS10345".to_string()),
            (262908u64, "10-1-LS10344".to_string()),
        ];
        assert_eq!(
            find_hwnd_for_plugin_name(&windows, "10-1-LS10344"),
            Some(262908u64),
            "应精确匹配 plugin_name 字符串，返回对应 HWND"
        );
    }

    #[test]
    fn test_find_hwnd_for_plugin_name_not_found() {
        let windows = vec![(197304u64, "10-2-LS10345".to_string())];
        assert_eq!(
            find_hwnd_for_plugin_name(&windows, "10-9-不存在"),
            None,
            "找不到对应窗口时应返回 None，不能误配到别的实例上——\
             那是比「点不到」更糟的「点错」"
        );
    }

    // ─────────────────────────────────────────────────────────────
    // 独立进程架构执行层（DEV-125986 第4步）：枚举 chrome.exe 命令行、
    // 从中找出目标 --user-data-dir 目录名对应的主进程 PID。
    // ─────────────────────────────────────────────────────────────

    #[test]
    fn test_build_list_chrome_main_processes_script_uses_cim_instance() {
        let script = build_list_chrome_main_processes_script();
        assert!(
            script.contains("Win32_Process"),
            "Get-Process 默认不返回命令行，必须用 CIM/WMI 才能读到 --user-data-dir"
        );
        assert!(
            script.contains("chrome.exe"),
            "只查 chrome.exe，不要把无关进程也扫进来"
        );
        assert!(
            script.contains("ProcessId") && script.contains("CommandLine"),
            "至少要带出 PID 和完整命令行，后续才能筛主进程、解析 user-data-dir"
        );
    }

    #[test]
    fn test_parse_chrome_main_processes_output_filters_subprocesses() {
        // 输出契约：每行 "<PID>|<CommandLine>"。子进程（带 --type=）应被
        // 过滤掉，不能把渲染进程的命令行也当成候选主进程
        let output = concat!(
            "7292|\"chrome.exe\" --user-data-dir=\"C:\\MyChromeData\\User4\" --flag-switches-begin\n",
            "3564|\"chrome.exe\" --type=renderer --extension-process --lang=zh-CN\n",
        );
        let result = parse_chrome_main_processes_output(output);
        assert_eq!(
            result,
            vec![(7292u32, "User4".to_string())],
            "应只保留主进程那一行，并已解析出 user-data-dir 目录名"
        );
    }

    #[test]
    fn test_parse_chrome_main_processes_output_skips_lines_without_user_data_dir() {
        // 主进程但没有 --user-data-dir 参数（如默认路径启动、未走独立
        // 数据目录方案）时，这条记录对建映射没有意义，应跳过而非报错
        let output = "7292|\"chrome.exe\" --flag-switches-begin --flag-switches-end\n";
        assert_eq!(
            parse_chrome_main_processes_output(output),
            Vec::<(u32, String)>::new(),
            "没有 --user-data-dir 的主进程不携带任何可用于识别的信息，应跳过"
        );
    }

    #[test]
    fn test_find_pid_for_user_data_dir_name_matches() {
        let processes = vec![
            (7292u32, "User4".to_string()),
            (8113u32, "User5".to_string()),
        ];
        assert_eq!(
            find_pid_for_user_data_dir_name(&processes, "User5"),
            Some(8113u32),
            "应按目录名精确匹配对应的 PID"
        );
    }

    #[test]
    fn test_find_pid_for_user_data_dir_name_not_found() {
        let processes = vec![(7292u32, "User4".to_string())];
        assert_eq!(
            find_pid_for_user_data_dir_name(&processes, "User9"),
            None,
            "映射表指向的目录名如果当前没有对应的运行中进程（比如该实例\
             Chrome 还没启动），应返回 None 而不是误配"
        );
    }

    // ─────────────────────────────────────────────────────────────
    // PID -> HWND（DEV-125986 第4步 part 2）：拿到目标 chrome.exe 主
    // 进程 PID 后，找到它对应的可见主窗口句柄。
    // ─────────────────────────────────────────────────────────────

    #[test]
    fn test_build_window_handle_for_pid_script_filters_by_id() {
        let script = build_window_handle_for_pid_script(7292);
        assert!(
            script.contains("7292"),
            "必须把目标 PID 拼进脚本，否则查的是全部 chrome.exe"
        );
        assert!(
            script.contains("MainWindowHandle"),
            "要拿到的正是这个进程的主窗口句柄"
        );
    }

    #[test]
    fn test_parse_window_handle_for_pid_output_valid() {
        assert_eq!(
            parse_window_handle_for_pid_output("197304"),
            Some(197304u64),
            "输出是单行句柄数字时应正确解析"
        );
    }

    #[test]
    fn test_parse_window_handle_for_pid_output_no_visible_window() {
        // 该 PID 存在但没有可见主窗口（如后台常驻、窗口已关闭）时，
        // PowerShell 侧 Get-Process 会返回空字符串
        assert_eq!(
            parse_window_handle_for_pid_output(""),
            None,
            "空输出（进程无可见主窗口）应返回 None，不能 panic"
        );
    }

    #[test]
    fn test_parse_window_handle_for_pid_output_zero_handle() {
        // MainWindowHandle 为 0 同样代表「没有主窗口」（Win32 惯例），
        // 不能当成一个合法句柄返回
        assert_eq!(
            parse_window_handle_for_pid_output("0"),
            None,
            "句柄为 0 代表没有主窗口，不是合法句柄"
        );
    }

    // ─────────────────────────────────────────────────────────────
    // HWND -> 窗口矩形（DEV-125986 第6步）：拿到目标窗口句柄后，查询
    // 它在屏幕上的矩形，传给图标定位脚本做区域限定。
    // ─────────────────────────────────────────────────────────────

    #[test]
    fn test_build_window_rect_script_uses_get_window_rect() {
        let script = build_window_rect_script(197304);
        assert!(script.contains("197304"), "必须把目标 HWND 拼进脚本");
        assert!(
            script.contains("GetWindowRect"),
            "必须调用 GetWindowRect 才能拿到虚拟桌面绝对坐标，\
             这跟脚本 --region 参数的坐标系是同一套"
        );
    }

    #[test]
    fn test_parse_window_rect_output_valid() {
        // 输出契约：一行 "left,top,right,bottom"（GetWindowRect 原生
        // 返回的是右下角坐标，不是宽高，需要在这里转换成 --region 要的
        // left,top,width,height 格式）
        assert_eq!(
            parse_window_rect_output("100,50,400,130"),
            Some((100, 50, 300, 80)),
            "应把 GetWindowRect 的 (left,top,right,bottom) 转成 \
             (left,top,width,height)"
        );
    }

    #[test]
    fn test_parse_window_rect_output_invalid_returns_none() {
        assert_eq!(
            parse_window_rect_output(""),
            None,
            "查询失败（如窗口已关闭）时应返回 None，不能 panic 或返回一个 (0,0,0,0) 的假区域"
        );
        assert_eq!(
            parse_window_rect_output("100,50,400"),
            None,
            "字段数不对时应返回 None，不能拼凑残缺数据"
        );
    }
}
