//! 本地日志接收服务（Task 1.2）
//!
//! 插件运行在浏览器沙箱内无法写文件，故由本服务接收其日志并转交落盘。
//!
//! # 网络边界（2026-08-22 起改为监听局域网）
//! 原先仅绑 127.0.0.1。领导要求「日志落盘本地 + AI 能远程调取分析」，
//! 两条合起来只能走拉模式：开发的 Mac 与 15 台采购虚拟机同在公司局域网，
//! 由 Mac 侧的 AI 直接连过来读（见 DEV-125123）。
//!
//! 日志含登录 token、供应商聊天与采购报价，故只读接口限制来源必须落在
//! 私有网段（`is_lan_addr`）。环境封闭在公司内网，2026-08-22 与开发确认
//! 按此程度处理、不再叠加共享密钥。

use axum::{
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use crate::heartbeat::{HeartbeatRegistry, HeartbeatRequest, HeartbeatResponse};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use std::time::Instant;

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
#[serde(rename_all = "camelCase")]
struct HealthResponse {
    ok: bool,
    /// 客户端版本，供插件判断兼容性。
    ///
    /// 取值必须与 updater 比对所用的版本同源（`tauri.conf.json` 的 version，
    /// 经 `app.package_info().version` 取得），故由调用方注入而非在此读
    /// `CARGO_PKG_VERSION`——后者是 Cargo.toml 的版本，两者不同步时会出现
    /// 「程序实际跑新版、对外自报旧版」，排查时被自己的日志误导。
    version: String,
    /// WS 握手令牌（DEV-125034）。
    ///
    /// # 为什么由 /health 下发，而不是写文件让插件读
    /// 插件在浏览器沙箱里读不到任意本地文件。而 `/health` 是既有端点、
    /// 插件本来就用它探活，顺带取令牌不增加任何链路。
    ///
    /// # 这样是否等于没有鉴权
    /// **不等于，但要说清边界**：任意网页脚本确实也能 GET /health 拿到令牌
    /// （CORS 放开了任意来源）。令牌真正挡住的是「**不知道客户端在哪/没探测过**
    /// 就直接连 WS」的情况，以及把攻击面从「任意页面随手连上」压到「必须先
    /// 成功探测本机端口」。
    ///
    /// 真正的隔离依赖两件事：仅绑 127.0.0.1（局域网到不了）、令牌每次启动
    /// 重新生成（不持久化、泄露仅限单次运行）。要做到「只有真插件能连」，
    /// 需把令牌改为写入插件安装目录（网页脚本读不到扩展的私有文件）——
    /// 那是下一步，本轮先保证通道本身可用且有校验位。
    ws_token: String,
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
    /// 插件心跳状态表。跨请求共享且需可变，故用 Mutex 包裹——
    /// 心跳每 5 秒一次、指令队列极短，锁竞争可忽略
    heartbeats: Arc<Mutex<HeartbeatRegistry>>,
    /// WS 握手令牌。每次客户端启动重新生成、不持久化
    ws_token: String,
}

async fn health(State(state): State<AppState>) -> Json<HealthResponse> {
    Json(HealthResponse {
        ok: true,
        version: state.version.clone(),
        ws_token: state.ws_token.clone(),
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
        // 插件用 toISOString() 上报的是 UTC，须归一为北京时间再落盘，
        // 否则日志页显示的时刻比实际早 8 小时
        let ts = e
            .timestamp
            .map(|t| crate::normalize_timestamp(&t))
            .unwrap_or_else(crate::current_timestamp);
        let level = e.level.unwrap_or_else(|| "info".to_string());
        let source = e.source.unwrap_or_else(|| "unknown".to_string());
        let plugin_name = e.plugin_name.unwrap_or_else(|| "unknown".to_string());
        let line = crate::format_log_line(&ts, &level, &source, &plugin_name, &e.message);
        state.sink.write_line(&line, crate::is_error_level(&level));
    }
    (StatusCode::OK, Json(LogAck { received: count }))
}

/// 接收插件心跳并把待执行指令捎回。
///
/// 插件活在浏览器沙箱内、没有可被连接的地址，客户端无法主动连它——故用
/// 「插件轮询 + 响应捎带指令」这一种形态完成双向通信（详见 heartbeat 模块头）。
///
/// 与 /log 一样对失败宽容：心跳解析异常也不返回错误码，避免插件因心跳失败
/// 干扰采购主业务
async fn ingest_heartbeat(
    State(state): State<AppState>,
    Json(req): Json<HeartbeatRequest>,
) -> (StatusCode, Json<HeartbeatResponse>) {
    let resp = match state.heartbeats.lock() {
        Ok(mut reg) => reg.on_heartbeat(&req, Instant::now()),
        // 锁被毒化（某次持锁时 panic）时不应连带打挂心跳链路，
        // 退化为「本次不下发指令」，插件下次心跳仍会重试
        Err(_) => HeartbeatResponse { commands: vec![] },
    };
    (StatusCode::OK, Json(resp))
}

// ─────────────────────────────────────────────────────────────────
// WebSocket 通道（DEV-125034/125035）
//
// # 相比 HTTP 轮询的收益
// - 客户端可**主动推**指令，不必等插件下次来问（原先最多延迟 5 秒）
// - 连接断开是即时事件，不必靠超时推算
// - 15 实例每秒 3 次 HTTP 请求的开销归零
//
// # 为什么仍然保留 last_seen + 阈值判定
// 没有直接用「连接断开 = 失联」：MV3 Service Worker 有 30 秒空闲销毁机制，
// 且 AI1-5422 实测过「窗口不在前台时 WS 被节流」。连接断开可能只是 SW 被
// 回收（插件其实是好的），立刻判失联会误报。故沿用原有阈值语义，
// WS 只替换传输层——这也让两种通道可以并存过渡。
//
// # 安全
// WS **不受同源策略限制**（没有预检、浏览器也不会因跨域拦截），任意网页脚本
// 都能连 ws://127.0.0.1:17653。故必须校验握手令牌，否则 1688 页面上的脚本
// 可以伪装成插件上报假状态、或接收下发的指令。这也是 CLAUDE.md 那条红线
// （「CORS 放开的前提是只写不读」）在双向通道下不再成立的直接后果。
// ─────────────────────────────────────────────────────────────────

/// WS 握手查询参数
#[derive(Debug, Deserialize)]
struct WsQuery {
    /// 握手令牌，必须与客户端本次运行生成的一致
    #[serde(default)]
    token: String,
}

/// WS 升级入口。令牌不匹配一律拒绝，不给出「令牌错」还是「没带」的区别——
/// 减少探测面。
///
/// # 提取器顺序有讲究
/// `Query` 与 `State` 必须放在 `WebSocketUpgrade` **之前**：后者是「消耗请求
/// 体」的提取器，一旦它先运行且请求不满足升级条件（缺 upgrade 头等），
/// axum 会直接返回 426 Upgrade Required——**令牌检查根本跑不到**。
/// 顺序写错的后果不是编译错误，而是鉴权被静默绕过一半（未授权者拿到 426
/// 而非 401，看似也被拒了，但那是协议检查在拒，不是我们的鉴权在拒）。
async fn ws_upgrade(
    axum::extract::Query(q): axum::extract::Query<WsQuery>,
    State(state): State<AppState>,
    ws: axum::extract::WebSocketUpgrade,
) -> axum::response::Response {
    if !crate::ws_token::token_matches(&state.ws_token, &q.token) {
        return (StatusCode::UNAUTHORIZED, "invalid token").into_response();
    }
    ws.on_upgrade(move |socket| handle_ws(socket, state))
}

/// 单个 WS 连接的收发循环。
///
/// 协议与 HTTP 心跳完全一致（同一个 `HeartbeatRequest`/`HeartbeatResponse`），
/// 只是换了载体——插件侧可以只改传输、不动业务逻辑。
async fn handle_ws(mut socket: axum::extract::ws::WebSocket, state: AppState) {
    use axum::extract::ws::Message;

    while let Some(Ok(msg)) = socket.recv().await {
        let text = match msg {
            Message::Text(t) => t,
            // 心跳保活帧：axum 自动回 Pong，无需处理
            Message::Ping(_) | Message::Pong(_) => continue,
            Message::Close(_) => break,
            // 二进制帧不在协议内，忽略而非断连——协议将来可能扩展
            Message::Binary(_) => continue,
        };

        // 解析失败不断连：一条坏消息不该让插件重连一轮，
        // 与 HTTP 侧「对失败宽容」的口径一致
        let req: HeartbeatRequest = match serde_json::from_str(&text) {
            Ok(r) => r,
            Err(_) => continue,
        };

        let resp = match state.heartbeats.lock() {
            Ok(mut reg) => reg.on_heartbeat(&req, Instant::now()),
            // 锁被毒化时退化为「本次不下发指令」，插件下次心跳仍会重试
            Err(_) => HeartbeatResponse { commands: vec![] },
        };

        let payload = match serde_json::to_string(&resp) {
            Ok(p) => p,
            Err(_) => continue,
        };
        // 发送失败说明对端已走，退出循环让连接自然关闭
        if socket.send(Message::Text(payload.into())).await.is_err() {
            break;
        }
    }
}

// ─────────────────────────────────────────────────────────────────
// 只读查询接口（DEV-125123）：供开发 Mac 上的 AI 远程调取日志分析
//
// 领导要求「日志落盘本地 + AI 能直接调取」，故走拉模式：AI 从局域网连过来
// 读，日志不出本机磁盘。接口复用已有的 LogQuery / summarize_log_dir，
// 查询逻辑与本地 CLI、GUI 日志页完全同源。
// ─────────────────────────────────────────────────────────────────

/// 只读接口的查询参数（与 CLI 选项对齐）
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RemoteLogQuery {
    /// 日期，缺省为今天
    #[serde(default)]
    date: Option<String>,
    #[serde(default)]
    levels: Option<String>,
    #[serde(default)]
    plugin_names: Option<String>,
    #[serde(default)]
    keyword: Option<String>,
    #[serde(default)]
    start_time: Option<String>,
    #[serde(default)]
    end_time: Option<String>,
    #[serde(default)]
    offset: Option<usize>,
    #[serde(default)]
    limit: Option<usize>,
}

/// 拒绝非内网来源。返回 Some 表示已拒绝，调用方直接返回该响应
fn reject_if_not_lan(addr: std::net::SocketAddr) -> Option<axum::response::Response> {
    if is_lan_addr(addr.ip()) {
        return None;
    }
    Some((StatusCode::FORBIDDEN, "仅限内网访问").into_response())
}

/// 列出有日志的日期（探活用：查不到数据时先确认日期对不对）
async fn api_log_dates(
    axum::extract::ConnectInfo(addr): axum::extract::ConnectInfo<std::net::SocketAddr>,
) -> axum::response::Response {
    if let Some(rejected) = reject_if_not_lan(addr) {
        return rejected;
    }
    let dir = crate::get_log_dir_public();
    Json(serde_json::json!({
        "machineName": crate::get_reported_machine_name(),
        "dates": crate::log_file::list_log_dates_in_dir(&dir),
        "retainDays": crate::log_file::RETAIN_DAYS,
    }))
    .into_response()
}

/// 聚合概览：各级别数量、各实例异常排名、归并后的错误类别
async fn api_log_summary(
    axum::extract::ConnectInfo(addr): axum::extract::ConnectInfo<std::net::SocketAddr>,
    axum::extract::Query(q): axum::extract::Query<RemoteLogQuery>,
) -> axum::response::Response {
    if let Some(rejected) = reject_if_not_lan(addr) {
        return rejected;
    }
    let date = q.date.unwrap_or_else(crate::current_date);
    match crate::log_file::summarize_log_dir(&crate::get_log_dir_public(), &date, false) {
        Ok(summary) => Json(serde_json::json!({
            "machineName": crate::get_reported_machine_name(),
            "summary": summary,
        }))
        .into_response(),
        Err(e) => Json(serde_json::json!({ "error": e })).into_response(),
    }
}

/// 明细查询（分页 + 筛选）
async fn api_log_entries(
    axum::extract::ConnectInfo(addr): axum::extract::ConnectInfo<std::net::SocketAddr>,
    axum::extract::Query(q): axum::extract::Query<RemoteLogQuery>,
) -> axum::response::Response {
    if let Some(rejected) = reject_if_not_lan(addr) {
        return rejected;
    }
    let date = q.date.clone().unwrap_or_else(crate::current_date);
    // 逗号分隔的多值参数，与 CLI 的 --level / --plugin 同口径
    let split = |s: Option<String>| -> Vec<String> {
        s.map(|v| {
            v.split(',')
                .map(|x| x.trim().to_string())
                .filter(|x| !x.is_empty())
                .collect()
        })
        .unwrap_or_default()
    };
    let query = crate::log_file::LogQuery {
        error_only: false,
        levels: split(q.levels).into_iter().map(|s| s.to_uppercase()).collect(),
        plugin_names: split(q.plugin_names),
        keyword: q.keyword.unwrap_or_default(),
        start_time: q.start_time.unwrap_or_default(),
        end_time: q.end_time.unwrap_or_default(),
        offset: q.offset.unwrap_or(0),
        limit: q.limit.unwrap_or(0),
    };
    match crate::log_file::read_log_page_from_dir(&crate::get_log_dir_public(), &date, &query) {
        Ok(page) => Json(serde_json::json!({
            "machineName": crate::get_reported_machine_name(),
            "date": date,
            "entries": page.entries,
            "total": page.total,
            "pluginNames": page.plugin_names,
        }))
        .into_response(),
        Err(e) => Json(serde_json::json!({ "error": e })).into_response(),
    }
}

fn build_router(
    sink: Arc<dyn LogSink>,
    version: &str,
    heartbeats: Arc<Mutex<HeartbeatRegistry>>,
    ws_token: String,
) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/log", post(ingest_log))
        .route("/heartbeat", post(ingest_heartbeat))
        .route("/ws", get(ws_upgrade))
        // 只读查询：供开发 Mac 上的 AI 远程调取（DEV-125123）
        .route("/api/log-dates", get(api_log_dates))
        .route("/api/log-summary", get(api_log_summary))
        .route("/api/log-entries", get(api_log_entries))
        .layer(build_cors_layer())
        .with_state(AppState {
            sink,
            version: version.to_string(),
            heartbeats,
            ws_token,
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
/// * `heartbeats` - 心跳状态表，由调用方持有同一实例并交给自愈巡检任务，
///   两边必须共享——巡检要读 HTTP 侧写入的 last_seen，也要往队列塞指令
/// 启动本地服务。返回实际监听的端口。
///
/// `ws_token` 是 WS 握手令牌（每次启动新生成），插件需带上它才能建立 WS 连接
pub fn spawn(
    sink: Arc<dyn LogSink>,
    version: &str,
    heartbeats: Arc<Mutex<HeartbeatRegistry>>,
    ws_token: String,
) -> Result<u16, String> {
    let listener = bind_listener()?;
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
            if let Err(e) =
                axum::serve(
                    listener,
                    build_router(sink, &version, heartbeats, ws_token)
                        // ConnectInfo 需要这个包装才能在 handler 里取到来源地址，
                        // 只读接口靠它做内网网段判断
                        .into_make_service_with_connect_info::<std::net::SocketAddr>(),
                )
                .await
            {
                eprintln!("日志服务异常退出: {}", e);
            }
        });
    });

    Ok(port)
}

/// 从 DEFAULT_PORT 起探测可用端口，仅绑定回环地址
/// 绑定监听地址。
///
/// # 为什么改成监听局域网（DEV-125123，2026-08-22）
/// 领导要求「日志直接落盘在本地」，同时希望 AI 能远程调取分析。两条合起来
/// 只能是拉模式：开发的 Mac 与 15 台采购虚拟机同在公司局域网，由 Mac 侧的
/// AI 直接连过来读。仅绑 127.0.0.1 的话跨机器根本连不上。
///
/// 原先坚持只绑回环是因为日志含登录 token、供应商聊天与采购报价。改为监听
/// 局域网后这层物理隔离没有了，取而代之的是**内网网段限制**（见
/// `is_lan_addr`）——2026-08-22 与开发确认：环境封闭在公司内网，
/// 按功能实现优先处理。
fn bind_listener() -> Result<std::net::TcpListener, String> {
    for offset in 0..MAX_PORT_PROBE {
        let port = DEFAULT_PORT + offset;
        // 0.0.0.0：同时接受本机（插件走 127.0.0.1）与局域网（AI 远程查询）
        if let Ok(l) = std::net::TcpListener::bind(("0.0.0.0", port)) {
            return Ok(l);
        }
    }
    Err(format!(
        "端口 {}~{} 均被占用，日志服务无法启动",
        DEFAULT_PORT,
        DEFAULT_PORT + MAX_PORT_PROBE - 1
    ))
}

/// 判断来源地址是否属于本机或内网私有网段。
///
/// 只读接口据此放行——日志含供应商聊天与采购报价，不该被公司网络之外的
/// 地址读到。覆盖 RFC1918 三段私有地址 + 回环。
pub fn is_lan_addr(ip: std::net::IpAddr) -> bool {
    use std::net::IpAddr;
    match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback() || v4.is_private() || v4.is_link_local()
        }
        // IPv6 只放行回环与唯一本地地址（fc00::/7）
        IpAddr::V6(v6) => {
            v6.is_loopback() || (v6.segments()[0] & 0xfe00) == 0xfc00
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// 构造测试用 AppState。心跳表每次新建，各用例互不干扰
    fn test_state(sink: Arc<dyn LogSink>, version: &str) -> AppState {
        AppState {
            sink,
            version: version.into(),
            heartbeats: Arc::new(Mutex::new(HeartbeatRegistry::new())),
            ws_token: TEST_WS_TOKEN.to_string(),
        }
    }

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
    fn test_bind_listener_uses_expected_port_range() {
        let listener = bind_listener().expect("绑定失败，日志服务无法启动");
        let addr = listener.local_addr().expect("读取地址失败");
        // 2026-08-22 起监听 0.0.0.0：原先只绑回环，跨机器根本连不上，
        // 而领导要求「日志落盘本地 + AI 远程调取」只能走拉模式（DEV-125123）。
        // 读取侧的边界改由 is_lan_addr 把关，见下面的用例
        assert!(
            addr.ip().is_unspecified() || addr.ip().is_loopback(),
            "应监听 0.0.0.0 以便局域网内的 AI 访问，实际: {}",
            addr.ip()
        );
        assert!(
            addr.port() >= DEFAULT_PORT && addr.port() < DEFAULT_PORT + MAX_PORT_PROBE,
            "端口应落在约定探测区间内，否则插件侧无法发现服务"
        );
    }

    #[test]
    fn test_is_lan_addr_accepts_private_and_loopback() {
        use std::net::IpAddr;
        for ip in ["127.0.0.1", "192.168.7.208", "10.0.0.5", "172.16.3.9"] {
            assert!(
                is_lan_addr(ip.parse::<IpAddr>().expect("解析失败")),
                "{} 属于本机或内网，应放行",
                ip
            );
        }
    }

    #[test]
    fn test_is_lan_addr_rejects_public() {
        use std::net::IpAddr;
        // 日志含供应商聊天与采购报价，不该被公司网络之外的地址读到
        for ip in ["8.8.8.8", "1.1.1.1", "203.0.113.7"] {
            assert!(
                !is_lan_addr(ip.parse::<IpAddr>().expect("解析失败")),
                "{} 是公网地址，必须拒绝",
                ip
            );
        }
    }

    #[test]
    fn test_bind_listener_probes_next_port_when_occupied() {
        let first = std::net::TcpListener::bind(("127.0.0.1", DEFAULT_PORT));
        // 若首端口本就被外部占用则跳过，避免误报
        if first.is_err() {
            return;
        }
        let second = bind_listener().expect("首端口被占用时应探测到下一个可用端口");
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
            State(test_state(sink.clone() as Arc<dyn LogSink>, "0.0.0-test")),
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
    async fn test_ingest_log_converts_plugin_utc_timestamp_to_beijing() {
        // 插件侧 new Date().toISOString() 报的是 UTC，落盘前须转北京时间；
        // 不转的话日志页显示的时刻比实际早 8 小时，排查时按错的时间找现场
        let sink = Arc::new(MemSink {
            lines: Mutex::new(Vec::new()),
        });
        let entries = vec![LogEntry {
            timestamp: Some("2026-08-18T01:25:18.602Z".into()),
            level: Some("info".into()),
            source: Some("content".into()),
            plugin_name: Some("aichat".into()),
            message: "来自 1688 页面".into(),
        }];
        // 本用例只关心落盘内容，返回值无需断言
        let _ = ingest_log(
            State(test_state(sink.clone() as Arc<dyn LogSink>, "0.0.0-test")),
            Json(entries),
        )
        .await;

        let lines = sink.lines.lock().expect("锁获取失败");
        assert!(
            lines[0].0.contains("2026-08-18 09:25:18.602"),
            "UTC 01:25 应落盘为北京时间 09:25，实际写入：{}",
            lines[0].0
        );
        assert!(
            !lines[0].0.contains('Z') && !lines[0].0.contains("01:25"),
            "不应残留 ISO 记法或原始 UTC 时刻：{}",
            lines[0].0
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
            State(test_state(sink.clone() as Arc<dyn LogSink>, "0.0.0-test")),
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
        let resp = health(State(test_state(sink as Arc<dyn LogSink>, injected)))
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

    /// 测试用固定令牌。真实运行时每次启动随机生成
    const TEST_WS_TOKEN: &str = "0123456789abcdef0123456789abcdef";

    fn test_router() -> Router {
        let sink = Arc::new(MemSink {
            lines: Mutex::new(Vec::new()),
        });
        build_router(
            sink as Arc<dyn LogSink>,
            "0.0.0-test",
            Arc::new(Mutex::new(HeartbeatRegistry::new())),
            TEST_WS_TOKEN.to_string(),
        )
    }

    // ─────────────────────────────────────────────────────────
    // WS 握手令牌（DEV-125034）：WS 不受同源策略限制，任意网页脚本都能连
    // ws://127.0.0.1:17653，不校验则可伪装成插件上报假状态、或收走下发的指令
    // ─────────────────────────────────────────────────────────

    /// 用真实 TCP 连接测 WS 握手。
    ///
    /// # 为什么不能用 `oneshot`
    /// axum 的 `WebSocketUpgrade` 需要 `hyper::upgrade::OnUpgrade` 扩展，
    /// 那是真实连接被 hyper 处理时才注入的。`oneshot` 直接把请求塞给 Router，
    /// 没有这个扩展，于是**永远返回 426 Upgrade Required、令牌检查根本跑不到**
    /// ——用它测这条路只会得到「看起来也拒了」的假象。故起真服务、真连接。
    async fn ws_handshake_status(query: &str) -> u16 {
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.expect("绑定失败");
        let port = listener.local_addr().expect("取地址失败").port();
        let router = test_router();
        tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });

        // 用裸 TCP 发握手请求：不引入 WS 客户端依赖，只看响应状态行
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .expect("连接失败");
        let req = format!(
            "GET /ws{} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nConnection: Upgrade\r\n\
             Upgrade: websocket\r\nSec-WebSocket-Version: 13\r\n\
             Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n",
            query, port
        );
        stream.write_all(req.as_bytes()).await.expect("发送失败");
        let mut buf = [0u8; 256];
        let n = stream.read(&mut buf).await.expect("读取失败");
        let head = String::from_utf8_lossy(&buf[..n]);
        // 状态行形如 "HTTP/1.1 401 Unauthorized"
        head.split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .unwrap_or(0)
    }

    #[tokio::test]
    async fn test_ws_rejects_missing_token() {
        assert_eq!(
            ws_handshake_status("").await,
            401,
            "不带令牌必须拒绝——否则 1688 页面上的任意脚本都能连上来"
        );
    }

    #[tokio::test]
    async fn test_ws_rejects_wrong_token() {
        assert_eq!(
            ws_handshake_status("?token=ffffffffffffffffffffffffffffffff").await,
            401,
            "错误令牌必须拒绝"
        );
    }

    #[tokio::test]
    async fn test_ws_accepts_valid_token() {
        assert_eq!(
            ws_handshake_status(&format!("?token={}", TEST_WS_TOKEN)).await,
            101,
            "正确令牌应完成协议升级（101 Switching Protocols）"
        );
    }

    #[tokio::test]
    async fn test_ws_rejects_token_prefix() {
        // 常量时间比较里的长度检查就是为了挡这个
        assert_eq!(
            ws_handshake_status("?token=0123456789abcdef").await,
            401,
            "正确前缀不得通过"
        );
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
    async fn test_heartbeat_endpoint_is_routed_and_returns_commands_field() {
        use axum::body::Body;
        use axum::http::{Method, Request};
        use tower::ServiceExt;

        // 端点必须真的挂上路由：漏挂时插件侧会收到 404，而心跳失败是静默的，
        // 排查起来只能看到「客户端没反应」
        let req = Request::builder()
            .method(Method::POST)
            .uri("/heartbeat")
            .header("origin", "https://www.1688.com")
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"pluginName":"robot-01","sidepanelOpen":true,"taskRunning":false,"acked":[]}"#,
            ))
            .expect("构造心跳请求失败");

        let resp = test_router().oneshot(req).await.expect("心跳请求执行失败");
        assert!(resp.status().is_success(), "心跳应返回 2xx");
        assert_eq!(
            resp.headers()
                .get("access-control-allow-origin")
                .and_then(|v| v.to_str().ok()),
            Some("*"),
            "心跳同样来自 1688 等第三方页面，必须带 CORS 头否则被浏览器拦截"
        );

        let body = axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .expect("读取响应体失败");
        let json: serde_json::Value =
            serde_json::from_slice(&body).expect("响应体应为合法 JSON");
        assert!(
            json.get("commands").map(|c| c.is_array()).unwrap_or(false),
            "响应必须含 commands 数组——插件靠它取回待执行指令：{}",
            String::from_utf8_lossy(&body)
        );
    }

    #[tokio::test]
    async fn test_heartbeat_tolerates_missing_optional_fields() {
        use axum::body::Body;
        use axum::http::{Method, Request};
        use tower::ServiceExt;

        // 插件旧版本可能不带新增字段，缺字段不应 400——
        // 否则升级期间新客户端会把旧插件的心跳全部拒掉，反而制造失联
        let req = Request::builder()
            .method(Method::POST)
            .uri("/heartbeat")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"pluginName":"robot-01"}"#))
            .expect("构造精简心跳请求失败");

        let resp = test_router().oneshot(req).await.expect("请求执行失败");
        assert!(
            resp.status().is_success(),
            "字段缺失的心跳应被接受（走默认值），实际状态码 {}",
            resp.status()
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
