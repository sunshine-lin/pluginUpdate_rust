//! 本地日志接收服务（Task 1.2）
//!
//! 插件运行在浏览器沙箱内无法写文件，故由本服务接收其日志并转交落盘。
//!
//! # 安全边界
//! **仅绑定 127.0.0.1**，不监听 0.0.0.0 —— AIChat 日志可能包含登录 token、
//! 供应商聊天内容与采购报价，暴露到局域网的风险高于插件本身崩溃。
//! 如需跨机器取日志（规划 Phase 3），必须同时补齐鉴权、脱敏与网段白名单。

use axum::{
    extract::State,
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// 起始端口。插件侧需与此一致；被占用时向后递增探测
pub const DEFAULT_PORT: u16 = 17653;
/// 端口探测的最大尝试次数
const MAX_PORT_PROBE: u16 = 10;

/// 单条日志条目（插件 POST /log 的数组元素）。
/// 插件侧（TypeScript）用驼峰命名字段，rename_all 使 serde 按驼峰匹配 JSON，
/// Rust 代码内仍用蛇形字段名——否则如 pluginName 这类复合词字段会静默解析失败。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LogEntry {
    /// 插件侧时间戳；缺失时由服务端补当前时间
    #[serde(default)]
    pub timestamp: Option<String>,
    #[serde(default)]
    pub level: Option<String>,
    /// 日志来源，如 background / sidepanel / content
    #[serde(default)]
    pub source: Option<String>,
    /// 插件名，标识具体是哪台机器/哪个采购账号；缺失时回落 unknown
    #[serde(default)]
    pub plugin_name: Option<String>,
    pub message: String,
}

#[derive(Debug, Serialize)]
struct HealthResponse {
    ok: bool,
    /// 客户端版本，供插件判断兼容性。
    ///
    /// 取值必须与 updater 比对所用的版本同源（`tauri.conf.json` 的 version，
    /// 经 `app.package_info().version` 取得），故由调用方注入而非在此读
    /// `CARGO_PKG_VERSION`——后者是 Cargo.toml 的版本，两者不同步时会出现
    /// 「程序实际跑新版、对外自报旧版」，排查时被自己的日志误导。
    version: String,
}

#[derive(Debug, Serialize)]
struct LogAck {
    received: usize,
}

/// 日志写入接口。Task 1.3 将以文件轮转实现替换当前的标准输出实现，
/// 抽象为 trait 以便 1.2 阶段即可独立联调、且写入策略可单测
pub trait LogSink: Send + Sync {
    fn write_line(&self, line: &str, is_error: bool);
}

/// 1.2 阶段的临时实现：仅打到标准输出，验证链路连通性
pub struct StdoutSink;

impl LogSink for StdoutSink {
    fn write_line(&self, line: &str, _is_error: bool) {
        println!("{}", line);
    }
}

#[derive(Clone)]
struct AppState {
    sink: Arc<dyn LogSink>,
    /// 对外自报的客户端版本，由调用方从 `app.package_info().version` 注入
    version: String,
}

async fn health(State(state): State<AppState>) -> Json<HealthResponse> {
    Json(HealthResponse {
        ok: true,
        version: state.version.clone(),
    })
}

/// 接收批量日志。约定返回 200 + 已接收条数；
/// 即使部分条目异常也不返回错误码 —— 插件侧不应因日志失败而干扰主业务
async fn ingest_log(
    State(state): State<AppState>,
    Json(entries): Json<Vec<LogEntry>>,
) -> (StatusCode, Json<LogAck>) {
    let count = entries.len();
    for e in entries {
        let ts = e
            .timestamp
            .unwrap_or_else(|| crate::current_timestamp());
        let level = e.level.unwrap_or_else(|| "info".to_string());
        let source = e.source.unwrap_or_else(|| "unknown".to_string());
        let plugin_name = e.plugin_name.unwrap_or_else(|| "unknown".to_string());
        let line = crate::format_log_line(&ts, &level, &source, &plugin_name, &e.message);
        state.sink.write_line(&line, crate::is_error_level(&level));
    }
    (StatusCode::OK, Json(LogAck { received: count }))
}

fn build_router(sink: Arc<dyn LogSink>, version: &str) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/log", post(ingest_log))
        .layer(build_cors_layer())
        .with_state(AppState {
            sink,
            version: version.to_string(),
        })
}

/// 构建 CORS 层。
///
/// 插件注入在 1688、CJ PMS 等第三方页面里运行，向 `127.0.0.1` 投递日志属
/// **跨域请求**：浏览器会先发 OPTIONS 预检，缺 CORS 头时直接拦截，日志一条
/// 都到不了本地（实测现象即插件侧报跨域错误）。
///
/// # 为何允许任意来源
/// 采购同事的插件未来可能跑在更多站点上，白名单方式每加一个站点就要改代码
/// 重新发版；本服务的暴露面也有限：
/// - 仅绑定 127.0.0.1，局域网内其它机器访问不到（见本文件顶部安全边界说明）
/// - 只提供「写入日志」与「查健康状态」，**不提供任何读取日志内容的接口**，
///   故放开来源不会导致已落盘的 token / 聊天记录 / 报价被第三方网页读走
///
/// 残留风险是任意网页可探测本机是否装了本更新器、并投递伪造日志行——
/// 相较于日志收不到导致排查无从下手，这个代价可以接受（2026-08-18 拍板）。
/// 若日后新增「读取日志」类接口，**必须重新收紧为来源白名单**。
fn build_cors_layer() -> tower_http::cors::CorsLayer {
    use tower_http::cors::{Any, CorsLayer};
    CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any)
}

/// 在后台线程启动日志服务，返回实际绑定的端口。
/// 绑定失败（端口全被占用）时返回 Err，调用方应降级为「不收集日志」而非阻断启动
///
/// # Arguments
/// * `sink` - 日志写入策略
/// * `version` - 对外自报的客户端版本，须传 `app.package_info().version`，
///   与 updater 比对所用版本同源
pub fn spawn(sink: Arc<dyn LogSink>, version: &str) -> Result<u16, String> {
    let listener = bind_loopback()?;
    let port = listener
        .local_addr()
        .map_err(|e| format!("读取监听地址失败: {}", e))?
        .port();

    let version = version.to_string();
    std::thread::spawn(move || {
        let rt = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(e) => {
                eprintln!("日志服务运行时创建失败: {}", e);
                return;
            }
        };
        rt.block_on(async move {
            listener
                .set_nonblocking(true)
                .expect("设置非阻塞失败，日志服务无法启动");
            let listener = match tokio::net::TcpListener::from_std(listener) {
                Ok(l) => l,
                Err(e) => {
                    eprintln!("日志服务监听转换失败: {}", e);
                    return;
                }
            };
            if let Err(e) = axum::serve(listener, build_router(sink, &version)).await {
                eprintln!("日志服务异常退出: {}", e);
            }
        });
    });

    Ok(port)
}

/// 从 DEFAULT_PORT 起探测可用端口，仅绑定回环地址
fn bind_loopback() -> Result<std::net::TcpListener, String> {
    for offset in 0..MAX_PORT_PROBE {
        let port = DEFAULT_PORT + offset;
        // 显式使用 127.0.0.1 而非 0.0.0.0：见模块头部安全说明
        if let Ok(l) = std::net::TcpListener::bind(("127.0.0.1", port)) {
            return Ok(l);
        }
    }
    Err(format!(
        "端口 {}~{} 均被占用，日志服务无法启动",
        DEFAULT_PORT,
        DEFAULT_PORT + MAX_PORT_PROBE - 1
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// 收集写入内容的测试替身
    struct MemSink {
        lines: Mutex<Vec<(String, bool)>>,
    }

    impl LogSink for MemSink {
        fn write_line(&self, line: &str, is_error: bool) {
            self.lines
                .lock()
                .expect("测试 sink 锁获取失败")
                .push((line.to_string(), is_error));
        }
    }

    #[test]
    fn test_bind_loopback_returns_listener_on_local_address() {
        let listener = bind_loopback().expect("绑定回环地址失败，日志服务无法启动");
        let addr = listener.local_addr().expect("读取地址失败");
        assert!(
            addr.ip().is_loopback(),
            "日志服务必须绑定回环地址；绑定 0.0.0.0 会将含 token 的日志暴露到局域网"
        );
        assert!(
            addr.port() >= DEFAULT_PORT && addr.port() < DEFAULT_PORT + MAX_PORT_PROBE,
            "端口应落在约定探测区间内，否则插件侧无法发现服务"
        );
    }

    #[test]
    fn test_bind_loopback_probes_next_port_when_occupied() {
        let first = std::net::TcpListener::bind(("127.0.0.1", DEFAULT_PORT));
        // 若首端口本就被外部占用则跳过，避免误报
        if first.is_err() {
            return;
        }
        let second = bind_loopback().expect("首端口被占用时应探测到下一个可用端口");
        assert_ne!(
            second.local_addr().expect("读取地址失败").port(),
            DEFAULT_PORT,
            "首端口已被占用，应返回不同端口，否则多实例场景下日志服务启动失败"
        );
    }

    #[test]
    fn test_log_entry_deserializes_from_camel_case_json() {
        // 插件侧（TypeScript）用驼峰命名 pluginName，若结构体不做 rename_all，
        // serde 精确匹配字段名会导致解析不到、静默回落 None——这是真实踩过的坑
        let json = r#"{"level":"error","source":"background","pluginName":"robot-01","message":"崩溃"}"#;
        let entry: LogEntry = serde_json::from_str(json).expect("应能解析插件侧实际发送的驼峰命名 JSON");
        assert_eq!(
            entry.plugin_name,
            Some("robot-01".to_string()),
            "pluginName（驼峰）应正确映射到 plugin_name 字段，否则日志里插件名永远是 unknown"
        );
    }

    #[tokio::test]
    async fn test_ingest_log_writes_each_entry_to_sink() {
        let sink = Arc::new(MemSink {
            lines: Mutex::new(Vec::new()),
        });
        let entries = vec![
            LogEntry {
                timestamp: Some("2026-07-29 10:00:00.000".into()),
                level: Some("error".into()),
                source: Some("background".into()),
                plugin_name: Some("robot-01".into()),
                message: "崩溃了".into(),
            },
            LogEntry {
                timestamp: Some("2026-07-29 10:00:01.000".into()),
                level: Some("info".into()),
                source: Some("sidepanel".into()),
                plugin_name: Some("robot-01".into()),
                message: "正常".into(),
            },
        ];
        let (status, ack) = ingest_log(
            State(AppState {
                sink: sink.clone() as Arc<dyn LogSink>,
                version: "0.0.0-test".into(),
            }),
            Json(entries),
        )
        .await;

        assert_eq!(status, StatusCode::OK, "日志接收应返回 200");
        assert_eq!(ack.received, 2, "应回报实际接收条数，供插件侧确认送达");

        let lines = sink.lines.lock().expect("锁获取失败");
        assert_eq!(lines.len(), 2, "两条日志都应写入 sink，漏写会导致排查时缺失线索");
        assert!(lines[0].1, "error 级别应标记为异常，供异常日志单独归集");
        assert!(!lines[1].1, "info 级别不应标记为异常");
        assert!(
            lines[0].0.contains("ERROR") && lines[0].0.contains("background"),
            "写入内容应为格式化后的完整日志行"
        );
        assert!(
            lines[0].0.contains("robot-01"),
            "写入内容应含插件名，否则多台机器日志混在一起无法区分来源"
        );
    }

    #[tokio::test]
    async fn test_ingest_log_fills_defaults_for_missing_fields() {
        let sink = Arc::new(MemSink {
            lines: Mutex::new(Vec::new()),
        });
        let entries = vec![LogEntry {
            timestamp: None,
            level: None,
            source: None,
            plugin_name: None,
            message: "字段缺失".into(),
        }];
        let (status, _) = ingest_log(
            State(AppState {
                sink: sink.clone() as Arc<dyn LogSink>,
                version: "0.0.0-test".into(),
            }),
            Json(entries),
        )
        .await;

        assert_eq!(
            status,
            StatusCode::OK,
            "字段缺失不应导致失败，否则插件侧一处遗漏就会丢掉整批日志"
        );
        let lines = sink.lines.lock().expect("锁获取失败");
        assert_eq!(lines.len(), 1, "缺字段的日志仍应落地");
        assert!(
            lines[0].0.contains("INFO") && lines[0].0.contains("unknown"),
            "缺失的级别/来源应回落为 INFO/unknown 而非丢弃该条"
        );
    }

    #[tokio::test]
    async fn test_health_reports_injected_version_not_cargo_version() {
        // 版本必须由调用方注入（来自 tauri.conf.json，与 updater 比对同源）。
        // 若此处退回 env!("CARGO_PKG_VERSION")，当两个文件版本不同步时
        // 会出现「实际跑新版、对外自报旧版」，排查时被自己的日志误导
        let sink = Arc::new(MemSink {
            lines: Mutex::new(Vec::new()),
        });
        let injected = "9.9.9-injected";
        let resp = health(State(AppState {
            sink: sink as Arc<dyn LogSink>,
            version: injected.into(),
        }))
        .await;

        assert!(resp.ok, "健康检查应返回 ok=true");
        assert_eq!(
            resp.version, injected,
            "/health 应回报注入的版本；返回 Cargo.toml 版本会与 updater 实际比对的版本脱节"
        );
        assert_ne!(
            resp.version,
            env!("CARGO_PKG_VERSION"),
            "本测试注入了与 Cargo 版本不同的值，若相等说明实现仍在读 CARGO_PKG_VERSION"
        );
    }

    // ─────────────────────────────────────────────────────────
    // CORS：插件运行在 1688 等第三方页面里，向 127.0.0.1 投递日志属跨域请求。
    // 缺少 CORS 头时浏览器直接拦截，日志一条都收不到（实测现象）
    // ─────────────────────────────────────────────────────────

    fn test_router() -> Router {
        let sink = Arc::new(MemSink {
            lines: Mutex::new(Vec::new()),
        });
        build_router(sink as Arc<dyn LogSink>, "0.0.0-test")
    }

    #[tokio::test]
    async fn test_preflight_options_on_log_is_allowed() {
        use axum::body::Body;
        use axum::http::{Method, Request};
        use tower::ServiceExt;

        // 浏览器在跨域 POST 前先发 OPTIONS 预检；没有 CORS 层时该方法未注册，
        // 会返回 405 导致真正的 POST 根本不会发出
        let req = Request::builder()
            .method(Method::OPTIONS)
            .uri("/log")
            .header("origin", "https://www.1688.com")
            .header("access-control-request-method", "POST")
            .header("access-control-request-headers", "content-type")
            .body(Body::empty())
            .expect("构造预检请求失败");

        let resp = test_router().oneshot(req).await.expect("预检请求执行失败");

        assert!(
            resp.status().is_success(),
            "OPTIONS 预检必须成功，收到 {}；失败时浏览器不会发出后续 POST",
            resp.status()
        );
        let headers = resp.headers();
        assert_eq!(
            headers
                .get("access-control-allow-origin")
                .and_then(|v| v.to_str().ok()),
            Some("*"),
            "预检响应需带 Allow-Origin: *，否则浏览器判定跨域被拒"
        );
        assert!(
            headers
                .get("access-control-allow-headers")
                .and_then(|v| v.to_str().ok())
                .map(|v| v.contains("content-type") || v == "*")
                .unwrap_or(false),
            "需允许 content-type 请求头，插件以 application/json 提交日志"
        );
    }

    #[tokio::test]
    async fn test_post_log_response_carries_cors_origin_header() {
        use axum::body::Body;
        use axum::http::{Method, Request};
        use tower::ServiceExt;

        // 预检通过后的实际 POST，其响应同样需要带 Allow-Origin，
        // 否则浏览器仍会拦下响应、插件侧看到的还是跨域错误
        let req = Request::builder()
            .method(Method::POST)
            .uri("/log")
            .header("origin", "https://www.1688.com")
            .header("content-type", "application/json")
            .body(Body::from(
                r#"[{"level":"info","source":"content","pluginName":"aichat","message":"跨域测试"}]"#,
            ))
            .expect("构造日志请求失败");

        let resp = test_router().oneshot(req).await.expect("日志请求执行失败");

        assert!(resp.status().is_success(), "日志写入应返回 2xx");
        assert_eq!(
            resp.headers()
                .get("access-control-allow-origin")
                .and_then(|v| v.to_str().ok()),
            Some("*"),
            "实际 POST 的响应也必须带 Allow-Origin，只在预检上带不够"
        );
    }
}
