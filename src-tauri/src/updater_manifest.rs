//! 桌面端自更新清单（latest.json）的构建与解析。
//!
//! 更新器自身不再靠人工替换文件升级：CI 出包后生成本模块定义的 latest.json，
//! 常驻的客户端定时拉取比对，版本更高即静默安装并重启。
//!
//! 这里只放**纯函数**——清单的拼装与解析。真正的下载、验签、安装由
//! tauri-plugin-updater 完成，不在本模块重复实现。
//!
//! 之所以要为清单单独写一层带测试的代码：清单由 CI 生成、被插件消费，
//! 字段名或结构写错时构建照样通过，直到十几台采购机器集体收不到更新才暴露。

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// tauri-plugin-updater 约定的平台标识（当前只分发 Windows x64）。
pub const PLATFORM_WINDOWS_X64: &str = "windows-x86_64";

/// 单个平台的更新产物：安装包地址 + 对应签名。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlatformEntry {
    /// `.sig` 文件的内容（minisign 签名），插件用它校验安装包未被篡改
    pub signature: String,
    /// 安装包下载地址
    pub url: String,
}

/// latest.json 的完整结构。字段名必须与 tauri-plugin-updater 的约定一致。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdaterManifest {
    pub version: String,
    /// 更新说明，展示给用户
    pub notes: String,
    /// RFC 3339 时间戳
    pub pub_date: String,
    pub platforms: BTreeMap<String, PlatformEntry>,
}

/// 构建仅含 Windows x64 的更新清单。
///
/// # Arguments
/// * `version` - 新版本号，不带 `v` 前缀
/// * `notes` - 更新说明
/// * `pub_date` - RFC 3339 时间戳
/// * `url` - 安装包完整下载地址
/// * `signature` - `.sig` 文件内容
pub fn build_windows_manifest(
    version: &str,
    notes: &str,
    pub_date: &str,
    url: &str,
    signature: &str,
) -> UpdaterManifest {
    let mut platforms = BTreeMap::new();
    platforms.insert(
        PLATFORM_WINDOWS_X64.to_string(),
        PlatformEntry {
            signature: signature.to_string(),
            url: url.to_string(),
        },
    );
    UpdaterManifest {
        version: version.to_string(),
        notes: notes.to_string(),
        pub_date: pub_date.to_string(),
        platforms,
    }
}

/// 序列化为 latest.json 文本（缩进两格，便于人工核对线上文件）。
pub fn manifest_to_json(manifest: &UpdaterManifest) -> Result<String, String> {
    serde_json::to_string_pretty(manifest).map_err(|e| format!("序列化更新清单失败: {}", e))
}

/// 解析 latest.json 文本。
///
/// 结构不符时返回 Err 而非 panic：清单由外部站点提供，
/// 内容异常只应导致「本次检查更新失败」，不能让常驻进程崩溃。
pub fn parse_manifest(text: &str) -> Result<UpdaterManifest, String> {
    serde_json::from_str(text).map_err(|e| format!("解析更新清单失败: {}", e))
}

/// 拼接 latest.json 的访问地址。
///
/// `base` 结尾有无 `/` 都能正确处理——CI 与配置分别由不同的人填写，
/// 多一条斜杠就 404 是这类拼接最常见的失败方式。
pub fn build_manifest_url(base: &str) -> String {
    format!("{}/latest.json", base.trim_end_matches('/'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_windows_manifest_fills_all_fields() {
        let m = build_windows_manifest(
            "0.2.0",
            "新增日志查看页面",
            "2026-08-01T10:00:00Z",
            "https://example.com/aichat-updater_0.2.0_x64-setup.exe",
            "dW50cnVzdGVkIGNvbW1lbnQ=",
        );

        assert_eq!(m.version, "0.2.0");
        assert_eq!(m.notes, "新增日志查看页面");
        assert_eq!(m.pub_date, "2026-08-01T10:00:00Z");

        let entry = m
            .platforms
            .get(PLATFORM_WINDOWS_X64)
            .expect("清单缺少 windows-x86_64 条目，Windows 客户端将收不到任何更新");
        assert_eq!(
            entry.url,
            "https://example.com/aichat-updater_0.2.0_x64-setup.exe"
        );
        assert_eq!(entry.signature, "dW50cnVzdGVkIGNvbW1lbnQ=");
    }

    #[test]
    fn test_manifest_json_uses_plugin_expected_field_names() {
        // 字段名与插件约定不一致时，插件会静默认为「无可用更新」，
        // 因此这里对 JSON 文本本身断言，而不只是对结构体断言
        let m = build_windows_manifest("1.0.0", "note", "2026-08-01T00:00:00Z", "https://e/x.exe", "sig");
        let json = manifest_to_json(&m).expect("序列化更新清单失败");

        assert!(json.contains("\"version\""), "缺少 version 字段: {}", json);
        assert!(json.contains("\"notes\""), "缺少 notes 字段: {}", json);
        assert!(json.contains("\"pub_date\""), "缺少 pub_date 字段: {}", json);
        assert!(json.contains("\"platforms\""), "缺少 platforms 字段: {}", json);
        assert!(
            json.contains("\"windows-x86_64\""),
            "平台键名必须是 windows-x86_64: {}",
            json
        );
        assert!(json.contains("\"signature\""), "缺少 signature 字段: {}", json);
        assert!(json.contains("\"url\""), "缺少 url 字段: {}", json);
    }

    #[test]
    fn test_manifest_roundtrip() {
        let original = build_windows_manifest(
            "0.3.1",
            "修复日志筛选",
            "2026-08-02T08:30:00Z",
            "https://example.com/setup.exe",
            "c2ln",
        );
        let json = manifest_to_json(&original).expect("序列化更新清单失败");
        let parsed = parse_manifest(&json).expect("回读自己生成的清单失败");
        assert_eq!(parsed, original, "生成与解析不对称，CI 产物将无法被客户端消费");
    }

    #[test]
    fn test_parse_manifest_accepts_real_plugin_format() {
        // 手写一份符合插件约定的清单，确认解析器不依赖自身序列化顺序
        let text = r#"{
            "version": "0.2.0",
            "notes": "test",
            "pub_date": "2026-08-01T10:00:00Z",
            "platforms": {
                "windows-x86_64": {
                    "signature": "abc",
                    "url": "https://example.com/a.exe"
                }
            }
        }"#;
        let m = parse_manifest(text).expect("无法解析符合插件约定的清单");
        assert_eq!(m.version, "0.2.0");
        assert_eq!(
            m.platforms.get(PLATFORM_WINDOWS_X64).map(|e| e.url.as_str()),
            Some("https://example.com/a.exe")
        );
    }

    #[test]
    fn test_parse_manifest_malformed_returns_err_not_panic() {
        // 站点返回 HTML 错误页 / 半截文件时，只应本次检查失败，不能让常驻进程崩溃
        assert!(parse_manifest("<html>502 Bad Gateway</html>").is_err());
        assert!(parse_manifest("").is_err());
        assert!(parse_manifest("{\"version\": \"1.0.0\"}").is_err(), "缺字段应报错");
    }

    #[test]
    fn test_build_manifest_url_handles_trailing_slash() {
        assert_eq!(
            build_manifest_url("https://chainai.cjdropshipping.cn/updater"),
            "https://chainai.cjdropshipping.cn/updater/latest.json"
        );
        assert_eq!(
            build_manifest_url("https://chainai.cjdropshipping.cn/updater/"),
            "https://chainai.cjdropshipping.cn/updater/latest.json",
            "base 末尾多一条斜杠不应产生双斜杠导致 404"
        );
    }
}
