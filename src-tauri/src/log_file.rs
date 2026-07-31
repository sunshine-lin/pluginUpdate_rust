//! 日志落盘与轮转（Task 1.3）
//!
//! 插件日志经 log_server 接收后由本模块写入磁盘，供事后排查与 AI 分析。
//!
//! # 容量约束
//! 采购同事多在**只有 C 盘的虚拟机**上办公，日志绝不能写满系统盘。
//! 故三重限制：按天分文件、单文件超 `MAX_FILE_BYTES` 切分、只保留
//! `RETAIN_DAYS` 天。超期文件在每日首次写入时清理。

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// 单文件大小上限，超过则切分为 `.1` `.2` 后缀
pub const MAX_FILE_BYTES: u64 = 10 * 1024 * 1024;
/// 日志保留天数，超期自动删除
pub const RETAIN_DAYS: i64 = 7;

const PREFIX: &str = "aichat";
const ERROR_PREFIX: &str = "aichat-error";

/// 构建全量日志文件名：`aichat-YYYY-MM-DD.log`
pub fn build_log_filename(date: &str) -> String {
    format!("{}-{}.log", PREFIX, date)
}

/// 构建异常日志文件名：`aichat-error-YYYY-MM-DD.log`
/// 异常单独归集，供 AI 直接读取而无需啃全量日志
pub fn build_error_log_filename(date: &str) -> String {
    format!("{}-{}.log", ERROR_PREFIX, date)
}

/// 构建切分后的文件名：`aichat-2026-07-29.1.log`
pub fn build_rotated_filename(base_name: &str, index: u32) -> String {
    let stem = base_name.strip_suffix(".log").unwrap_or(base_name);
    format!("{}.{}.log", stem, index)
}

/// 判断是否需要切分：现有大小 + 本次写入量超过上限即切分
pub fn should_rotate(current_size: u64, incoming_len: u64) -> bool {
    current_size + incoming_len > MAX_FILE_BYTES
}

/// 从日志文件名中解析日期，非日志文件返回 None。
/// 同时识别全量、异常与切分文件（`aichat-2026-07-29.1.log`）
pub fn parse_date_from_filename(name: &str) -> Option<String> {
    let stem = name.strip_suffix(".log")?;
    // 去掉切分序号后缀（.1 / .2）
    let stem = match stem.rsplit_once('.') {
        Some((head, tail)) if tail.chars().all(|c| c.is_ascii_digit()) => head,
        _ => stem,
    };
    let date = stem
        .strip_prefix(&format!("{}-", ERROR_PREFIX))
        .or_else(|| stem.strip_prefix(&format!("{}-", PREFIX)))?;
    // 校验 YYYY-MM-DD 形状，避免把无关文件误判为日志而删除
    let parts: Vec<&str> = date.split('-').collect();
    if parts.len() == 3
        && parts[0].len() == 4
        && parts[1].len() == 2
        && parts[2].len() == 2
        && parts.iter().all(|p| p.chars().all(|c| c.is_ascii_digit()))
    {
        Some(date.to_string())
    } else {
        None
    }
}

/// 判断某日志日期是否已超出保留期
pub fn is_expired(file_date: &str, today: &str, retain_days: i64) -> bool {
    let parse = |s: &str| chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").ok();
    match (parse(file_date), parse(today)) {
        // 日期无法解析时保守保留，不误删
        (Some(f), Some(t)) => (t - f).num_days() >= retain_days,
        _ => false,
    }
}

/// 清理目录中超期的日志文件，返回删除的文件数。
/// 仅处理符合命名规则的日志文件，其它文件一律不动
pub fn cleanup_expired_logs(dir: &Path, today: &str, retain_days: i64) -> usize {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return 0,
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if let Some(date) = parse_date_from_filename(&name) {
            if is_expired(&date, today, retain_days) && fs::remove_file(entry.path()).is_ok() {
                removed += 1;
            }
        }
    }
    removed
}

/// 在目录中为 base_name 找到当前应写入的文件路径（考虑切分）
fn resolve_target_path(dir: &Path, base_name: &str, incoming_len: u64) -> PathBuf {
    let primary = dir.join(base_name);
    let size = fs::metadata(&primary).map(|m| m.len()).unwrap_or(0);
    if !should_rotate(size, incoming_len) {
        return primary;
    }
    // 主文件已满，找第一个未满的切分文件
    for idx in 1..u32::MAX {
        let candidate = dir.join(build_rotated_filename(base_name, idx));
        let sz = fs::metadata(&candidate).map(|m| m.len()).unwrap_or(0);
        if !should_rotate(sz, incoming_len) {
            return candidate;
        }
    }
    primary
}

/// 文件型日志写入器：按天分文件 + 超限切分 + 超期清理
pub struct FileSink {
    dir: PathBuf,
    /// 记录上次执行清理的日期，避免每条日志都扫目录
    last_cleanup_date: Mutex<Option<String>>,
}

impl FileSink {
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            last_cleanup_date: Mutex::new(None),
        }
    }

    /// 追加一行到指定文件，失败仅告警不 panic —— 日志故障不应影响主流程
    fn append(&self, filename: &str, line: &str) {
        if fs::create_dir_all(&self.dir).is_err() {
            return;
        }
        let payload_len = line.len() as u64 + 1;
        let path = resolve_target_path(&self.dir, filename, payload_len);
        match OpenOptions::new().create(true).append(true).open(&path) {
            Ok(mut f) => {
                if let Err(e) = writeln!(f, "{}", line) {
                    eprintln!("写入日志文件失败 {:?}: {}", path, e);
                }
            }
            Err(e) => eprintln!("打开日志文件失败 {:?}: {}", path, e),
        }
    }

    /// 每天首次写入时清理超期文件
    fn cleanup_once_per_day(&self, today: &str) {
        let mut last = match self.last_cleanup_date.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        if last.as_deref() == Some(today) {
            return;
        }
        cleanup_expired_logs(&self.dir, today, RETAIN_DAYS);
        *last = Some(today.to_string());
    }
}

impl crate::log_server::LogSink for FileSink {
    fn write_line(&self, line: &str, is_error: bool) {
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        self.cleanup_once_per_day(&today);
        self.append(&build_log_filename(&today), line);
        // 异常额外写入独立文件，供 AI 直读；全量文件仍保留该条以维持上下文完整
        if is_error {
            self.append(&build_error_log_filename(&today), line);
        }
    }
}

// ─────────────────────────────────────────────────────────────────
// 日志查看页面支持（读取侧，纯函数 + 可测试实现）
// ─────────────────────────────────────────────────────────────────

/// 已解析的单条日志，供前端展示
#[derive(Debug, Clone, serde::Serialize)]
pub struct LogEntry {
    pub timestamp: String,
    pub level: String,
    pub source: String,
    pub plugin_name: String,
    pub message: String,
}

/// 解析一行日志。新格式（新增插件名字段后）：
/// `[时间戳] [级别] [来源] [插件名] 消息`；旧格式（历史遗留，日志只保留 7 天，
/// 迁移窗口期很短）：`[时间戳] [级别] [来源] 消息`，插件名回落 "unknown"。
/// 格式不符返回 None（跳过而非报错），避免个别损坏行导致整个文件读取失败。
///
/// 新旧格式判定：解析完"来源"字段后，若紧跟的是 `[`（还有下一个方括号字段）
/// 则视为新格式、继续解析插件名；若紧跟空格则是旧格式。这是基于语法结构的
/// 判定，不是猜测——但消息本身也可能以方括号开头（如 "[Slider] ..."），
/// 此时无法与"新格式插件名"区分，会被当作插件名解析掉、丢失该前缀。
/// 这是已知的、可接受的历史数据局限（仅影响迁移窗口期内产生的旧格式日志）。
pub fn parse_log_line(line: &str) -> Option<LogEntry> {
    let rest = line.strip_prefix('[')?;
    let (timestamp, rest) = rest.split_once("] [")?;
    let (level, rest) = rest.split_once("] [")?;
    let (source, rest) = rest.split_once("] ")?;

    if let Some(after_bracket) = rest.strip_prefix('[') {
        if let Some((plugin_name, message)) = after_bracket.split_once("] ") {
            return Some(LogEntry {
                timestamp: timestamp.to_string(),
                level: level.to_string(),
                source: source.to_string(),
                plugin_name: plugin_name.to_string(),
                message: message.to_string(),
            });
        }
    }

    Some(LogEntry {
        timestamp: timestamp.to_string(),
        level: level.to_string(),
        source: source.to_string(),
        plugin_name: "unknown".to_string(),
        message: rest.to_string(),
    })
}

/// 扫描目录中所有日志文件（含异常与切分文件），提取去重后按日期倒序排列的日期列表，
/// 供前端下拉框选择。逻辑与 cleanup_expired_logs 共享 parse_date_from_filename。
pub fn list_log_dates_in_dir(dir: &Path) -> Vec<String> {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };
    let mut dates: Vec<String> = entries
        .flatten()
        .filter_map(|e| parse_date_from_filename(&e.file_name().to_string_lossy()))
        .collect();
    dates.sort();
    dates.dedup();
    dates.reverse();
    dates
}

/// 读取指定日期的日志条目。error_only=true 时只读异常文件，否则读全量文件
/// （含切分文件按序号依次拼接）。文件不存在返回空列表而非报错——用户选择
/// 无日志的日期是正常场景，不应让页面报错。
pub fn read_log_entries_from_dir(
    dir: &Path,
    date: &str,
    error_only: bool,
) -> Result<Vec<LogEntry>, String> {
    let base_name = if error_only {
        build_error_log_filename(date)
    } else {
        build_log_filename(date)
    };

    let mut entries = Vec::new();
    // 主文件 + 切分文件（.1 .2 ...）依次读取，遇到不存在的序号即停止
    let mut candidates = vec![dir.join(&base_name)];
    for idx in 1..u32::MAX {
        let rotated = dir.join(build_rotated_filename(&base_name, idx));
        if !rotated.exists() {
            break;
        }
        candidates.push(rotated);
    }

    for path in candidates {
        let content = match fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => continue, // 文件不存在或读取失败：跳过而非整体报错
        };
        entries.extend(content.lines().filter_map(parse_log_line));
    }

    Ok(entries)
}

/// 从已读取的日志条目中提取去重后的插件名列表，按字母排序，
/// 供前端筛选下拉框展示可选项。直接复用已读入内存的 entries，
/// 不再单独扫一次文件。
pub fn collect_plugin_names(entries: &[LogEntry]) -> Vec<String> {
    entries
        .iter()
        .map(|e| e.plugin_name.clone())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log_server::LogSink;
    use tempfile::TempDir;

    #[test]
    fn test_build_filenames() {
        assert_eq!(build_log_filename("2026-07-29"), "aichat-2026-07-29.log");
        assert_eq!(
            build_error_log_filename("2026-07-29"),
            "aichat-error-2026-07-29.log",
            "异常日志文件名需与全量区分，否则 AI 无法只读异常"
        );
        assert_eq!(
            build_rotated_filename("aichat-2026-07-29.log", 1),
            "aichat-2026-07-29.1.log",
            "切分文件名格式变动会导致清理逻辑无法识别、旧文件永不删除"
        );
    }

    #[test]
    fn test_should_rotate_only_when_exceeding_limit() {
        assert!(
            !should_rotate(0, 100),
            "空文件写入小内容不应切分，否则会产生大量碎片文件"
        );
        assert!(!should_rotate(MAX_FILE_BYTES - 10, 10), "刚好达到上限不应切分");
        assert!(
            should_rotate(MAX_FILE_BYTES, 1),
            "超过上限必须切分，否则单文件无限增长会写满采购虚拟机的 C 盘"
        );
    }

    #[test]
    fn test_parse_date_from_filename_recognizes_all_log_forms() {
        assert_eq!(
            parse_date_from_filename("aichat-2026-07-29.log"),
            Some("2026-07-29".to_string())
        );
        assert_eq!(
            parse_date_from_filename("aichat-error-2026-07-29.log"),
            Some("2026-07-29".to_string()),
            "异常日志需可解析日期，否则超期后不会被清理"
        );
        assert_eq!(
            parse_date_from_filename("aichat-2026-07-29.3.log"),
            Some("2026-07-29".to_string()),
            "切分文件需可解析日期，否则大文件永久堆积"
        );
    }

    #[test]
    fn test_parse_date_rejects_unrelated_files() {
        assert_eq!(
            parse_date_from_filename("config.json"),
            None,
            "非日志文件必须返回 None，否则清理逻辑会删除用户其它文件"
        );
        assert_eq!(parse_date_from_filename("aichat-notadate.log"), None);
        assert_eq!(
            parse_date_from_filename("other-2026-07-29.log"),
            None,
            "前缀不符的文件不应被识别为本应用日志"
        );
    }

    #[test]
    fn test_is_expired_boundary() {
        assert!(
            !is_expired("2026-07-29", "2026-07-29", 7),
            "当天日志不应过期"
        );
        assert!(!is_expired("2026-07-23", "2026-07-29", 7), "第 6 天仍在保留期内");
        assert!(
            is_expired("2026-07-22", "2026-07-29", 7),
            "满 7 天应过期，否则日志无限累积"
        );
    }

    #[test]
    fn test_is_expired_keeps_file_when_date_unparseable() {
        assert!(
            !is_expired("bad-date", "2026-07-29", 7),
            "日期无法解析时应保守保留，避免误删"
        );
    }

    #[test]
    fn test_cleanup_removes_only_expired_logs() {
        let tmp = TempDir::new().expect("创建临时目录失败，会导致清理测试无法运行");
        let dir = tmp.path();
        fs::write(dir.join("aichat-2026-07-01.log"), "old").expect("写入过期日志失败");
        fs::write(dir.join("aichat-error-2026-07-01.log"), "old").expect("写入过期异常日志失败");
        fs::write(dir.join("aichat-2026-07-29.log"), "new").expect("写入当天日志失败");
        fs::write(dir.join("config.json"), "{}").expect("写入无关文件失败");

        let removed = cleanup_expired_logs(dir, "2026-07-29", RETAIN_DAYS);

        assert_eq!(removed, 2, "应删除 2 个过期日志文件");
        assert!(
            dir.join("aichat-2026-07-29.log").exists(),
            "当天日志被误删，会导致正在排查的问题线索丢失"
        );
        assert!(
            dir.join("config.json").exists(),
            "无关文件被删除，会破坏用户配置——清理逻辑必须只认自己的命名规则"
        );
    }

    #[test]
    fn test_file_sink_writes_line_to_dated_file() {
        let tmp = TempDir::new().expect("创建临时目录失败");
        let sink = FileSink::new(tmp.path().to_path_buf());
        sink.write_line("[ts] [INFO] [background] hello", false);

        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        let content = fs::read_to_string(tmp.path().join(build_log_filename(&today)))
            .expect("当天日志文件应存在，否则日志未真正落盘");
        assert!(content.contains("hello"), "写入内容应出现在日志文件中");
        assert!(content.ends_with('\n'), "每条日志应以换行结尾以保证一行一条");
    }

    #[test]
    fn test_file_sink_appends_instead_of_overwriting() {
        let tmp = TempDir::new().expect("创建临时目录失败");
        let sink = FileSink::new(tmp.path().to_path_buf());
        sink.write_line("first", false);
        sink.write_line("second", false);

        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        let content = fs::read_to_string(tmp.path().join(build_log_filename(&today)))
            .expect("日志文件应存在");
        assert!(
            content.contains("first") && content.contains("second"),
            "后写入的日志覆盖了先前内容，会导致只剩最后一条、排查失去历史"
        );
    }

    #[test]
    fn test_file_sink_writes_error_to_both_files() {
        let tmp = TempDir::new().expect("创建临时目录失败");
        let sink = FileSink::new(tmp.path().to_path_buf());
        sink.write_line("[ts] [ERROR] [background] 崩溃", true);

        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        let all = fs::read_to_string(tmp.path().join(build_log_filename(&today)))
            .expect("全量日志应存在");
        let err = fs::read_to_string(tmp.path().join(build_error_log_filename(&today)))
            .expect("异常日志文件应存在，否则 AI 无法只读异常");
        assert!(all.contains("崩溃"), "异常也应保留在全量日志中以维持上下文");
        assert!(err.contains("崩溃"), "异常应写入独立文件");
    }

    #[test]
    fn test_file_sink_does_not_write_info_to_error_file() {
        let tmp = TempDir::new().expect("创建临时目录失败");
        let sink = FileSink::new(tmp.path().to_path_buf());
        sink.write_line("[ts] [INFO] [background] 正常", false);

        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        assert!(
            !tmp.path().join(build_error_log_filename(&today)).exists(),
            "INFO 日志不应创建异常文件，否则异常文件被噪声淹没、失去筛选价值"
        );
    }

    #[test]
    fn test_resolve_target_path_switches_to_rotated_file_when_full() {
        let tmp = TempDir::new().expect("创建临时目录失败");
        let dir = tmp.path();
        let base = "aichat-2026-07-29.log";
        // 造一个已达上限的主文件
        fs::write(dir.join(base), vec![b'x'; MAX_FILE_BYTES as usize])
            .expect("预置满容量日志文件失败");

        let target = resolve_target_path(dir, base, 10);
        assert_eq!(
            target.file_name().and_then(|n| n.to_str()),
            Some("aichat-2026-07-29.1.log"),
            "主文件已满时应写入切分文件，否则单文件无限增长"
        );
    }

    // ─────────────────────────────────────────────────────────
    // 日志查看页面（Rust 侧）：解析日志行 / 枚举可选日期 / 读取当天日志
    // ─────────────────────────────────────────────────────────

    #[test]
    fn test_parse_log_line_extracts_all_fields() {
        let entry = parse_log_line(
            "[2026-07-29 10:30:00.123] [ERROR] [background] [robot-01] service worker 崩溃",
        )
        .expect("标准格式日志行应能解析成功");
        assert_eq!(entry.timestamp, "2026-07-29 10:30:00.123");
        assert_eq!(entry.level, "ERROR");
        assert_eq!(entry.source, "background");
        assert_eq!(entry.plugin_name, "robot-01");
        assert_eq!(entry.message, "service worker 崩溃");
    }

    #[test]
    fn test_parse_log_line_handles_message_containing_brackets() {
        // 消息本身含 [Slider] 前缀，解析不应在消息内的方括号处提前截断
        let entry = parse_log_line(
            "[2026-07-29 10:30:00.123] [WARN] [content] [robot-01] [Slider] 命中1688风控硬拦截",
        )
        .expect("消息含方括号时仍应正确解析");
        assert_eq!(
            entry.message, "[Slider] 命中1688风控硬拦截",
            "消息中的方括号不应被误认为字段分隔符，否则显示内容被截断"
        );
    }

    #[test]
    fn test_parse_log_line_rejects_malformed_lines() {
        assert!(
            parse_log_line("这不是一条日志").is_none(),
            "格式不符的行应返回 None，否则会在页面上显示乱码字段"
        );
        assert!(
            parse_log_line("").is_none(),
            "空行应返回 None，避免读取整日文件时因末尾空行报错"
        );
    }

    #[test]
    fn test_parse_log_line_falls_back_to_legacy_three_field_format() {
        // 新增插件名字段前落盘的历史日志是旧格式（无插件名），必须仍可解析，
        // 否则用户过去几天的日志会在新页面里全部消失
        let entry = parse_log_line("[2026-07-29 10:30:00.123] [ERROR] [background] service worker 崩溃")
            .expect("旧格式（无插件名）日志行应仍可解析，否则历史日志全部无法显示");
        assert_eq!(entry.timestamp, "2026-07-29 10:30:00.123");
        assert_eq!(entry.level, "ERROR");
        assert_eq!(entry.source, "background");
        assert_eq!(
            entry.plugin_name, "unknown",
            "旧格式日志没有插件名字段，应回落 unknown 而非解析失败"
        );
        assert_eq!(entry.message, "service worker 崩溃");
    }

    #[test]
    fn test_parse_log_line_new_format_message_with_brackets_still_parses_correctly() {
        // 新格式日志（含插件名）的消息本身含方括号前缀（如 [Slider]），
        // 只解析前 4 段，其余整体作为 message，不会因消息内方括号而错位
        let entry = parse_log_line(
            "[2026-07-29 10:30:00.123] [WARN] [content] [robot-01] [Slider] 命中1688风控硬拦截",
        )
        .expect("新格式日志应能解析");
        assert_eq!(entry.plugin_name, "robot-01");
        assert_eq!(entry.message, "[Slider] 命中1688风控硬拦截");
    }

    #[test]
    fn test_list_log_dates_deduplicates_and_sorts_descending() {
        let tmp = TempDir::new().expect("创建临时目录失败，会导致日期列表测试无法运行");
        let dir = tmp.path();
        fs::write(dir.join("aichat-2026-07-27.log"), "a").expect("写入失败");
        fs::write(dir.join("aichat-error-2026-07-27.log"), "a").expect("写入失败");
        fs::write(dir.join("aichat-2026-07-29.log"), "a").expect("写入失败");
        fs::write(dir.join("aichat-2026-07-28.1.log"), "a").expect("写入失败");
        fs::write(dir.join("config.json"), "{}").expect("写入无关文件失败");

        let dates = list_log_dates_in_dir(dir);

        assert_eq!(
            dates,
            vec!["2026-07-29", "2026-07-28", "2026-07-27"],
            "日期应去重（全量+异常同日只出现一次）且按最新优先排序，否则前端下拉框日期错乱或重复"
        );
    }

    #[test]
    fn test_read_log_entries_from_dir_skips_unparseable_lines() {
        let tmp = TempDir::new().expect("创建临时目录失败");
        let dir = tmp.path();
        let content = "[2026-07-29 10:00:00.000] [INFO] [background] 正常\n\
                        损坏的行\n\
                        [2026-07-29 10:00:01.000] [ERROR] [content] 出错了\n";
        fs::write(dir.join("aichat-2026-07-29.log"), content).expect("写入失败");

        let entries = read_log_entries_from_dir(dir, "2026-07-29", false)
            .expect("读取应成功，不应因个别损坏行而整体失败");

        assert_eq!(
            entries.len(),
            2,
            "应跳过损坏行、只返回可解析的 2 条，损坏行不应导致整体读取失败或计数错误"
        );
        assert_eq!(entries[0].message, "正常");
        assert_eq!(entries[1].message, "出错了");
    }

    #[test]
    fn test_read_log_entries_error_only_reads_error_file() {
        let tmp = TempDir::new().expect("创建临时目录失败");
        let dir = tmp.path();
        fs::write(
            dir.join("aichat-2026-07-29.log"),
            "[2026-07-29 10:00:00.000] [INFO] [background] 正常\n",
        )
        .expect("写入全量日志失败");
        fs::write(
            dir.join("aichat-error-2026-07-29.log"),
            "[2026-07-29 10:00:01.000] [ERROR] [content] 出错了\n",
        )
        .expect("写入异常日志失败");

        let entries = read_log_entries_from_dir(dir, "2026-07-29", true)
            .expect("异常日志文件应能正常读取");

        assert_eq!(
            entries.len(),
            1,
            "error_only 模式应只读异常文件，不应混入全量日志内容"
        );
        assert_eq!(entries[0].message, "出错了");
    }

    #[test]
    fn test_read_log_entries_missing_file_returns_empty_not_error() {
        let tmp = TempDir::new().expect("创建临时目录失败");
        let entries = read_log_entries_from_dir(tmp.path(), "2099-01-01", false)
            .expect("文件不存在时应返回空列表而非报错，避免用户选择无日志的日期时页面报错崩溃");
        assert!(entries.is_empty());
    }

    #[test]
    fn test_collect_plugin_names_deduplicates_and_sorts() {
        let entries = vec![
            LogEntry {
                timestamp: "t1".into(),
                level: "INFO".into(),
                source: "sidepanel".into(),
                plugin_name: "robot-02".into(),
                message: "m1".into(),
            },
            LogEntry {
                timestamp: "t2".into(),
                level: "ERROR".into(),
                source: "background".into(),
                plugin_name: "robot-01".into(),
                message: "m2".into(),
            },
            LogEntry {
                timestamp: "t3".into(),
                level: "INFO".into(),
                source: "sidepanel".into(),
                plugin_name: "robot-02".into(),
                message: "m3".into(),
            },
        ];
        assert_eq!(
            collect_plugin_names(&entries),
            vec!["robot-01".to_string(), "robot-02".to_string()],
            "插件名应去重且按字母排序，否则前端下拉框选项重复或顺序不稳定"
        );
    }

    #[test]
    fn test_collect_plugin_names_empty_entries_returns_empty() {
        assert!(collect_plugin_names(&[]).is_empty());
    }
}
