//! 日志查询 CLI（DEV-125123）
//!
//! # 为什么把查询能力做成 CLI
//! 领导要求「插件出问题时 AI 能直接调取日志辅助分析」。现有查询能力只服务
//! Tauri 前端（走 `invoke`），AI 拿不到。CLI + stdout JSON 是最简形态：
//! 不开网络端口、不需要鉴权、任何 AI 工具都能调。
//!
//! # 为什么复用同一个二进制而非独立 CLI
//! 日志目录解析（`get_log_dir()` 有三平台分支）、日志行解析规则、脱敏口径
//! 都在本 crate 里。独立 CLI 要么重复实现这些、要么把 crate 拆库，
//! 都不如加一个子命令。
//!
//! # Windows 上的 stdout 陷阱
//! `main.rs` 有 `#![windows_subsystem = "windows"]`（防 GUI 启动时弹控制台
//! 黑窗），后果是**进程没有附加控制台，stdout 写不到调用方**。故 Windows 下
//! 必须先 `AttachConsole` 到父进程控制台，否则 CLI 输出会静默消失——
//! 这不是「输出为空」，是根本没写出去。见 `attach_console_if_needed()`。

use crate::log_file::{self, LogQuery};
use std::path::Path;

/// CLI 子命令名。作为第一个参数出现时进入 CLI 模式、不拉起 GUI
pub const QUERY_SUBCOMMAND: &str = "query-logs";

/// 解析后的查询请求
#[derive(Debug, PartialEq)]
pub enum CliRequest {
    /// 列出有日志的日期（探活用：查不到数据时先确认日期对不对）
    ListDates,
    /// 聚合概览
    Summary { date: String },
    /// 明细查询
    Entries { date: String, query: LogQuery },
    /// 参数有误，携带给用户看的说明
    Usage(String),
}

/// 解析命令行参数（纯函数，不碰文件系统，便于测试）。
///
/// `args` 不含程序名与子命令名，即 `["--date", "2026-08-22", ...]`。
///
/// 缺 `--date` 时默认「今天」由调用方补——本函数不读时钟，保持可测。
pub fn parse_args(args: &[String], today: &str) -> CliRequest {
    let mut date = today.to_string();
    let mut summary = false;
    let mut dates = false;
    let mut q = LogQuery::default();

    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        // 取下一个值型参数，缺失时报错而非静默用空串——
        // `--date` 后面漏了值会导致查了个空日期却什么都不说
        let mut next = |name: &str| -> Result<String, String> {
            i += 1;
            args.get(i)
                .cloned()
                .ok_or_else(|| format!("参数 {} 缺少值", name))
        };
        let r = match arg {
            "--dates" => {
                dates = true;
                Ok(())
            }
            "--summary" => {
                summary = true;
                Ok(())
            }
            "--error-only" => {
                q.error_only = true;
                Ok(())
            }
            "--date" => next("--date").map(|v| date = v),
            "--level" => next("--level").map(|v| {
                // 逗号分隔支持多级别：--level ERROR,WARN
                q.levels = v.split(',').map(|s| s.trim().to_uppercase()).collect();
            }),
            "--plugin" => next("--plugin").map(|v| {
                q.plugin_names = v.split(',').map(|s| s.trim().to_string()).collect();
            }),
            "--keyword" => next("--keyword").map(|v| q.keyword = v),
            "--from" => next("--from").map(|v| q.start_time = v),
            "--to" => next("--to").map(|v| q.end_time = v),
            "--offset" => next("--offset").and_then(|v| {
                v.parse()
                    .map(|n| q.offset = n)
                    .map_err(|_| format!("--offset 需要整数，收到: {}", v))
            }),
            "--limit" => next("--limit").and_then(|v| {
                v.parse()
                    .map(|n| q.limit = n)
                    .map_err(|_| format!("--limit 需要整数，收到: {}", v))
            }),
            "--help" | "-h" => return CliRequest::Usage(usage_text()),
            other => Err(format!("未知参数: {}", other)),
        };
        if let Err(e) = r {
            return CliRequest::Usage(format!("{}\n\n{}", e, usage_text()));
        }
        i += 1;
    }

    if dates {
        return CliRequest::ListDates;
    }
    if summary {
        return CliRequest::Summary { date };
    }
    CliRequest::Entries { date, query: q }
}

/// 用法说明。同时是给 AI 看的接口文档，故把「先 summary 再收窄」的用法
/// 顺序写进去——不这么提示，AI 容易一上来就拉全量
pub fn usage_text() -> String {
    format!(
        "用法: aichat-updater {} [选项]

选项:
  --dates                列出有日志的日期（查不到数据时先用它探活）
  --summary              输出聚合概览：各级别数量、各实例异常排名、归并后的错误类别
  --date <YYYY-MM-DD>    指定日期，默认今天
  --level <A,B>          按级别筛选，逗号分隔，如 ERROR,WARN
  --plugin <A,B>         按实例名筛选，逗号分隔
  --keyword <词>         关键词（大小写不敏感，匹配消息+来源+级别+实例名）
  --from <HH:MM>         起始时间（北京时间）
  --to <HH:MM>           结束时间
  --error-only           只读异常日志文件（aichat-error-*.log）
  --offset <N>           跳过前 N 条命中
  --limit <N>            本页条数，默认 {}，上限 {}

建议顺序: 先 --summary 看全局（哪个实例、哪类错误最多），再按实例或关键词收窄查明细。
不要一上来就拉全量——一天可达几十万行。

日志只保留 {} 天，查更早的日期必然为空（不代表当时没问题）。",
        QUERY_SUBCOMMAND,
        log_file::DEFAULT_PAGE_LIMIT,
        log_file::MAX_PAGE_LIMIT,
        log_file::RETAIN_DAYS,
    )
}

/// 执行请求并返回要打印到 stdout 的字符串。
///
/// 错误也序列化成 JSON 返回（而非 panic 或裸文本）：调用方是程序/AI，
/// 拿到结构化的 `{"error": ...}` 才能判断，裸文本还得靠猜
/// 同 `execute`，但在结果里带上机器标识。
///
/// 将来多台机器的输出汇总到一处时，没有它无法区分数据来自哪台——
/// 而 `plugin_name` 只能区分同机的实例（各实例下载目录不同），跨机器会撞名
pub fn execute_with_machine(req: CliRequest, log_dir: &Path, machine_id: &str) -> String {
    let body = execute(req, log_dir);
    // 用法说明不是 JSON，原样返回
    if !body.trim_start().starts_with('{') {
        return body;
    }
    match serde_json::from_str::<serde_json::Value>(&body) {
        Ok(mut v) => {
            if let Some(obj) = v.as_object_mut() {
                obj.insert(
                    "machineId".to_string(),
                    serde_json::Value::String(machine_id.to_string()),
                );
            }
            json_or_error(&v)
        }
        // 解析不回来时原样返回，不因为加字段失败而丢掉查询结果
        Err(_) => body,
    }
}

pub fn execute(req: CliRequest, log_dir: &Path) -> String {
    match req {
        CliRequest::Usage(text) => text,
        CliRequest::ListDates => {
            let dates = log_file::list_log_dates_in_dir(log_dir);
            json_or_error(&serde_json::json!({
                "dates": dates,
                "logDir": log_dir.to_string_lossy(),
                "retainDays": log_file::RETAIN_DAYS,
            }))
        }
        CliRequest::Summary { date } => {
            // 走全量扫描而非分页读取：拿一页（上限 2000 条）去聚合，等于用
            // 0.5% 的样本算「哪个实例异常最多」——排名不可信，而输出看起来
            // 却像正经统计，比不给更危险。聚合只累加计数，内存与日志量无关
            match log_file::summarize_log_dir(log_dir, &date, false) {
                Ok(summary) => json_or_error(&serde_json::json!({ "summary": summary })),
                Err(e) => json_or_error(&serde_json::json!({ "error": e })),
            }
        }
        CliRequest::Entries { date, query } => {
            match log_file::read_log_page_from_dir(log_dir, &date, &query) {
                Ok(page) => json_or_error(&serde_json::json!({
                    "date": date,
                    "entries": page.entries,
                    "total": page.total,
                    "pluginNames": page.plugin_names,
                })),
                Err(e) => json_or_error(&serde_json::json!({ "error": e })),
            }
        }
    }
}

/// 序列化失败时退化为一条错误 JSON，不 panic——CLI 崩溃比返回错误难排查
fn json_or_error(v: &serde_json::Value) -> String {
    serde_json::to_string_pretty(v)
        .unwrap_or_else(|e| format!("{{\"error\":\"结果序列化失败: {}\"}}", e))
}

/// Windows 下把 stdout 接到父进程的控制台。
///
/// `main.rs` 的 `windows_subsystem = "windows"` 让 GUI 启动时不弹黑窗，
/// 代价是进程默认没有控制台，`println!` 写进虚空。CLI 模式下必须先附加，
/// 否则表现为「命令跑完没有任何输出」——极易被误判成程序没执行。
#[cfg(target_os = "windows")]
pub fn attach_console_if_needed() {
    // ATTACH_PARENT_PROCESS = u32::MAX，即 (DWORD)-1
    #[link(name = "kernel32")]
    extern "system" {
        fn AttachConsole(dwProcessId: u32) -> i32;
    }
    unsafe {
        // 失败（如从无控制台的环境启动）时不做处理：
        // 调用方若重定向了 stdout 仍能拿到输出
        AttachConsole(u32::MAX);
    }
}

#[cfg(not(target_os = "windows"))]
pub fn attach_console_if_needed() {
    // 其他平台的 stdout 本来就通，无需处理
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn test_parse_defaults_to_entries_with_today() {
        let r = parse_args(&[], "2026-08-22");
        match r {
            CliRequest::Entries { date, query } => {
                assert_eq!(date, "2026-08-22", "缺 --date 时应默认今天");
                assert_eq!(query.limit, 0, "未指定 limit 时保持 0，由读取层归一到默认值");
            }
            other => panic!("应解析为明细查询，实际: {:?}", other),
        }
    }

    #[test]
    fn test_parse_dates_flag() {
        assert_eq!(parse_args(&args(&["--dates"]), "2026-08-22"), CliRequest::ListDates);
    }

    #[test]
    fn test_parse_summary_flag() {
        assert_eq!(
            parse_args(&args(&["--summary", "--date", "2026-08-20"]), "2026-08-22"),
            CliRequest::Summary {
                date: "2026-08-20".to_string()
            }
        );
    }

    #[test]
    fn test_parse_multi_level_and_plugin() {
        let r = parse_args(
            &args(&["--level", "error,WARN", "--plugin", "robot-01,robot-07"]),
            "2026-08-22",
        );
        match r {
            CliRequest::Entries { query, .. } => {
                assert_eq!(
                    query.levels,
                    vec!["ERROR", "WARN"],
                    "级别应统一大写，避免大小写不一致漏筛"
                );
                assert_eq!(query.plugin_names, vec!["robot-01", "robot-07"]);
            }
            other => panic!("实际: {:?}", other),
        }
    }

    #[test]
    fn test_parse_missing_value_reports_usage() {
        // `--date` 后漏了值不能静默当空日期查，否则查了个空结果却什么都不说
        match parse_args(&args(&["--date"]), "2026-08-22") {
            CliRequest::Usage(msg) => assert!(msg.contains("--date"), "应指出是哪个参数缺值"),
            other => panic!("缺值应返回用法说明，实际: {:?}", other),
        }
    }

    #[test]
    fn test_parse_unknown_arg_reports_usage() {
        match parse_args(&args(&["--nonexistent"]), "2026-08-22") {
            CliRequest::Usage(msg) => assert!(msg.contains("未知参数")),
            other => panic!("实际: {:?}", other),
        }
    }

    #[test]
    fn test_parse_non_numeric_limit_reports_usage() {
        match parse_args(&args(&["--limit", "很多"]), "2026-08-22") {
            CliRequest::Usage(msg) => assert!(msg.contains("--limit")),
            other => panic!("实际: {:?}", other),
        }
    }

    #[test]
    fn test_usage_text_mentions_retention_and_order() {
        // 用法说明同时是给 AI 看的接口文档，两个关键约束必须在里面
        let t = usage_text();
        assert!(t.contains("--summary"), "须说明先看概览");
        assert!(
            t.contains(&log_file::RETAIN_DAYS.to_string()),
            "须说明只保留 7 天，否则查空会被误判成没问题"
        );
    }

    #[test]
    fn test_execute_list_dates_returns_json() {
        let tmp = TempDir::new().expect("临时目录");
        fs::write(
            tmp.path().join(log_file::build_log_filename("2026-08-22")),
            "[2026-08-22 10:00:00.000] [INFO] [background] [robot-01] 正常\n",
        )
        .expect("写入");

        let out = execute(CliRequest::ListDates, tmp.path());
        let v: serde_json::Value = serde_json::from_str(&out).expect("输出须是合法 JSON");
        assert_eq!(v["dates"][0], "2026-08-22");
        assert_eq!(v["retainDays"], log_file::RETAIN_DAYS);
    }

    #[test]
    fn test_execute_summary_aggregates() {
        let tmp = TempDir::new().expect("临时目录");
        fs::write(
            tmp.path().join(log_file::build_log_filename("2026-08-22")),
            "[2026-08-22 10:00:00.000] [ERROR] [background] [robot-07] 商品 111 抓取失败\n\
             [2026-08-22 10:00:01.000] [ERROR] [background] [robot-07] 商品 222 抓取失败\n\
             [2026-08-22 10:00:02.000] [INFO] [background] [robot-01] 正常\n",
        )
        .expect("写入");

        let out = execute(
            CliRequest::Summary {
                date: "2026-08-22".to_string(),
            },
            tmp.path(),
        );
        let v: serde_json::Value = serde_json::from_str(&out).expect("合法 JSON");
        assert_eq!(v["summary"]["total"], 3);
        assert_eq!(
            v["summary"]["byPlugin"][0]["pluginName"], "robot-07",
            "异常最多的实例应排最前"
        );
        assert_eq!(
            v["summary"]["topErrors"][0]["count"], 2,
            "只有 ID 不同的两条应归并为一类"
        );
    }

    #[test]
    fn test_execute_summary_scans_beyond_page_limit() {
        // 聚合必须扫全量：拿一页（上限 2000）去统计等于用样本算排名，
        // 「哪个实例异常最多」会不可信，而输出看起来却像正经统计
        let tmp = TempDir::new().expect("临时目录");
        let over = log_file::MAX_PAGE_LIMIT + 500;
        let mut content = String::new();
        for i in 0..over {
            content.push_str(&format!(
                "[2026-08-22 10:00:00.000] [INFO] [background] [robot-01] 第{}条\n",
                i
            ));
        }
        fs::write(
            tmp.path().join(log_file::build_log_filename("2026-08-22")),
            content,
        )
        .expect("写入");

        let out = execute(
            CliRequest::Summary {
                date: "2026-08-22".to_string(),
            },
            tmp.path(),
        );
        let v: serde_json::Value = serde_json::from_str(&out).expect("合法 JSON");
        assert_eq!(
            v["summary"]["total"], over,
            "聚合须统计全部条目，不得受分页上限约束"
        );
    }

    #[test]
    fn test_execute_entries_returns_filtered() {
        let tmp = TempDir::new().expect("临时目录");
        fs::write(
            tmp.path().join(log_file::build_log_filename("2026-08-22")),
            "[2026-08-22 10:00:00.000] [ERROR] [background] [robot-01] 崩了\n\
             [2026-08-22 10:00:01.000] [INFO] [background] [robot-01] 正常\n",
        )
        .expect("写入");

        let req = parse_args(&args(&["--level", "ERROR"]), "2026-08-22");
        let out = execute(req, tmp.path());
        let v: serde_json::Value = serde_json::from_str(&out).expect("合法 JSON");
        assert_eq!(v["total"], 1, "筛选应在后端生效");
        assert_eq!(v["entries"][0]["message"], "崩了");
    }

    #[test]
    fn test_execute_with_machine_injects_id() {
        let tmp = TempDir::new().expect("临时目录");
        let out = execute_with_machine(CliRequest::ListDates, tmp.path(), "win-buyer01-a1b2c3d4");
        let v: serde_json::Value = serde_json::from_str(&out).expect("合法 JSON");
        assert_eq!(
            v["machineId"], "win-buyer01-a1b2c3d4",
            "输出须带机器标识，否则多台汇总时无法区分来源"
        );
    }

    #[test]
    fn test_execute_with_machine_passes_usage_through() {
        // 用法说明不是 JSON，不该被当成 JSON 处理后丢掉
        let tmp = TempDir::new().expect("临时目录");
        let out = execute_with_machine(
            CliRequest::Usage("用法说明文本".to_string()),
            tmp.path(),
            "m-1",
        );
        assert_eq!(out, "用法说明文本");
    }

    #[test]
    fn test_execute_missing_date_returns_empty_not_error() {
        // 用户查一个没有日志的日期是正常场景，不该报错
        let tmp = TempDir::new().expect("临时目录");
        let out = execute(
            CliRequest::Entries {
                date: "2099-01-01".to_string(),
                query: LogQuery::default(),
            },
            tmp.path(),
        );
        let v: serde_json::Value = serde_json::from_str(&out).expect("合法 JSON");
        assert_eq!(v["total"], 0);
        assert!(v["error"].is_null(), "无日志不应报错");
    }
}
