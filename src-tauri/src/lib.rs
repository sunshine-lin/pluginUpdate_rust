use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{Read, Write};
use std::path::PathBuf;

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

/// 自定义路径配置文件结构，区分 online / test 环境
#[derive(Debug, Default, Serialize, Deserialize)]
struct PathConfig {
    online_path: Option<String>,
    test_path: Option<String>,
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
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            get_app_info,
            check_update,
            perform_update,
            get_saved_path,
            save_custom_path,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
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
}
