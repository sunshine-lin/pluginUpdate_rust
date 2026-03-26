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

/// 获取当前环境（test 或 online）
fn get_env() -> String {
    option_env!("APP_ENV").unwrap_or("online").to_string()
}

/// 根据环境获取下载 URL
fn get_download_url() -> String {
    let env = get_env();
    if env == "test" {
        "http://cj-chain-ai.cjdropshipping.offline.pre.cn/aichat.zip".to_string()
    } else {
        "https://chainai.cjdropshipping.cn/aichat.zip".to_string()
    }
}

/// 获取安装路径
fn get_install_path() -> PathBuf {
    let env = get_env();
    let folder_name = if env == "test" {
        "aichat_test"
    } else {
        "aichat"
    };

    if cfg!(target_os = "windows") {
        PathBuf::from(format!("D:\\{}", folder_name))
    } else {
        // macOS: ~/aichat 或 ~/aichat_test
        let mac_folder = if env == "test" { "aichat_test" } else { "aichat" };
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join(mac_folder)
    }
}

/// 构建 HTTP 客户端（测试环境禁用 SSL 验证）
fn build_http_client() -> Result<reqwest::Client, reqwest::Error> {
    let builder = reqwest::Client::builder();
    if get_env() == "test" {
        builder.danger_accept_invalid_certs(true).build()
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

/// 获取应用信息
#[tauri::command]
fn get_app_info() -> UpdateInfo {
    let install_path = get_install_path();
    let current_version = get_local_version(&install_path);
    UpdateInfo {
        install_path: install_path.to_string_lossy().to_string(),
        current_version,
        env: get_env(),
        download_url: get_download_url(),
    }
}

/// 检查更新（对比本地与远程版本）
#[tauri::command]
async fn check_update() -> Result<CheckResult, String> {
    let install_path = get_install_path();
    let current_version = get_local_version(&install_path);
    let download_url = get_download_url();

    // 尝试从 zip 同级目录获取 manifest.json 进行版本对比
    let manifest_url = download_url.replace("aichat.zip", "manifest.json");

    let client = build_http_client().map_err(|e| format!("创建HTTP客户端失败: {}", e))?;

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
async fn perform_update() -> Result<String, String> {
    let install_path = get_install_path();
    let download_url = get_download_url();

    // 创建安装目录
    fs::create_dir_all(&install_path)
        .map_err(|e| format!("创建目录失败: {}", e))?;

    // 下载 ZIP 文件到临时目录
    let temp_dir = tempfile::tempdir()
        .map_err(|e| format!("创建临时目录失败: {}", e))?;
    let zip_path = temp_dir.path().join("aichat.zip");

    let client = build_http_client().map_err(|e| format!("创建HTTP客户端失败: {}", e))?;

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

    // 解压 ZIP 文件
    let file = fs::File::open(&zip_path)
        .map_err(|e| format!("打开ZIP文件失败: {}", e))?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|e| format!("解析ZIP文件失败: {}", e))?;

    for i in 0..archive.len() {
        let mut file = archive
            .by_index(i)
            .map_err(|e| format!("读取ZIP条目失败: {}", e))?;

        let file_name = file.name().to_string();
        // 安全检查：防止路径穿越
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
        .invoke_handler(tauri::generate_handler![
            get_app_info,
            check_update,
            perform_update
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
