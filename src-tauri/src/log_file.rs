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

/// 日志查询条件（筛选 + 分页）。
///
/// # 为什么筛选必须下推到后端
/// 一台采购机器会跑 3~15 个 Chrome 实例，全部实例的日志灌进同一个文件。
/// 原实现把当天全部条目读进内存、整体 IPC 传给前端、前端再过滤并全量渲染
/// DOM——15 倍日志量下三层各自都是瓶颈（读 GB 级文本、序列化百万对象、
/// 渲染几十万节点），点开日志页必然卡死。
///
/// 分页放前端做不行：日志页的用途是排查问题，筛选必须覆盖当天全部数据，
/// 只在「前端已拿到的那一页」里筛等于筛不到。故筛选与分页都下推到这里，
/// 在 parse 阶段就把不命中的丢掉，只把命中的那一页返回。
#[derive(Debug, Default, Clone, PartialEq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LogQuery {
    /// 只读异常（ERROR/WARN）文件
    #[serde(default)]
    pub error_only: bool,
    /// 级别白名单，空表示不限
    #[serde(default)]
    pub levels: Vec<String>,
    /// 插件名白名单，空表示不限
    #[serde(default)]
    pub plugin_names: Vec<String>,
    /// 关键词，大小写不敏感。匹配范围沿用原前端行为：消息 + 来源 + 级别 +
    /// 插件名拼接后整体包含匹配（不只匹配消息），空表示不限
    #[serde(default)]
    pub keyword: String,
    /// 起始时间，`HH:MM` 形式（沿用原前端筛选控件的粒度），空表示不限
    #[serde(default)]
    pub start_time: String,
    /// 结束时间，`HH:MM` 形式，空表示不限
    #[serde(default)]
    pub end_time: String,
    /// 跳过的命中条数
    #[serde(default)]
    pub offset: usize,
    /// 本页最多返回的条数。0 视为使用默认值，避免前端漏传导致退回全量读取
    #[serde(default)]
    pub limit: usize,
}

/// 单页返回条数上限的默认值。取 500 是因为日志表格一屏撑不过百行，
/// 500 条已足够翻阅，再多只是白付序列化与渲染成本
pub const DEFAULT_PAGE_LIMIT: usize = 500;

/// 单页返回条数的硬上限。前端传再大也截断到此值——放开上限等于允许
/// 退回「一次性全量返回」，本次改动要解决的正是那个问题
pub const MAX_PAGE_LIMIT: usize = 2000;

/// 分页查询结果
#[derive(Debug, serde::Serialize)]
pub struct LogPage {
    /// 本页条目
    pub entries: Vec<LogEntry>,
    /// 命中筛选条件的总条数（不受分页限制），供前端显示「共 X 条」
    pub total: usize,
    /// 当天出现过的全部插件名（不受筛选影响，供筛选下拉框列选项）
    pub plugin_names: Vec<String>,
}

/// 从时间戳里取出 `HH:MM`，供时间段筛选比较。
///
/// 时间戳可能是 `YYYY-MM-DD HH:MM:SS.mmm`，也可能是历史遗留的其它形态，
/// 故按「第一个 HH:MM 模式」提取而非按固定下标切——与原前端正则同口径。
fn extract_hour_minute(timestamp: &str) -> Option<String> {
    let bytes = timestamp.as_bytes();
    for i in 0..bytes.len().saturating_sub(4) {
        if bytes[i].is_ascii_digit()
            && bytes[i + 1].is_ascii_digit()
            && bytes[i + 2] == b':'
            && bytes[i + 3].is_ascii_digit()
            && bytes[i + 4].is_ascii_digit()
        {
            return Some(timestamp[i..i + 5].to_string());
        }
    }
    None
}

/// 判断单条日志是否命中筛选条件（不含分页）。
///
/// 筛选语义与改造前的前端过滤保持一致，避免用户感到「筛选行为变了」：
/// 级别与插件名是白名单、关键词匹配「消息+来源+级别+插件名」拼接串、
/// 时间段按 `HH:MM` 粒度比较（定宽零填充，字典序即时间序）。
pub fn entry_matches(entry: &LogEntry, q: &LogQuery) -> bool {
    // 级别比较统一转大写：文件里可能是 error/ERROR 混用
    if !q.levels.is_empty()
        && !q
            .levels
            .iter()
            .any(|l| l.eq_ignore_ascii_case(&entry.level))
    {
        return false;
    }
    if !q.plugin_names.is_empty() && !q.plugin_names.iter().any(|p| p == &entry.plugin_name) {
        return false;
    }
    if !q.keyword.trim().is_empty() {
        // 大小写不敏感：排查时不该因为大小写记错而搜不到
        let hay = format!(
            "{}{}{}{}",
            entry.message, entry.source, entry.level, entry.plugin_name
        )
        .to_lowercase();
        if !hay.contains(&q.keyword.trim().to_lowercase()) {
            return false;
        }
    }
    if !q.start_time.is_empty() || !q.end_time.is_empty() {
        // 取不到 HH:MM 的条目在设了时间段时一律不命中——宁可少给，
        // 也不要把无法判定时间的条目混进用户指定的时间窗里
        let hm = match extract_hour_minute(&entry.timestamp) {
            Some(v) => v,
            None => return false,
        };
        if !q.start_time.is_empty() && hm.as_str() < q.start_time.as_str() {
            return false;
        }
        if !q.end_time.is_empty() && hm.as_str() > q.end_time.as_str() {
            return false;
        }
    }
    true
}

/// 把 limit 归一到有效区间：0（前端漏传）取默认值，超上限截断
pub fn normalize_limit(limit: usize) -> usize {
    if limit == 0 {
        DEFAULT_PAGE_LIMIT
    } else {
        limit.min(MAX_PAGE_LIMIT)
    }
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
/// 把读到的时间戳归一为北京时间。
///
/// # 为什么读取侧也要归一
/// 归一本来只在写入时做（`log_server::ingest_log`）。那意味着**老客户端落盘的行
/// 永远是插件上报的原始 UTC** —— 同一个文件里两种格式混着，而时间段筛选按
/// 「第一个 HH:MM」取值当北京时间比较，对 UTC 那批会整体偏 8 小时。
/// 2026-08-24 在 10 号机实测到：同一天 74000 行里，13:47 装上新版之前全是 UTC。
///
/// 只对含 `T` 的 ISO 形态调用转换函数，避免在几十万行的全量扫描里做无用的解析尝试
/// （`--summary` 要扫全量，那条路径对每行的开销都敏感）。
fn normalize_read_timestamp(raw: &str) -> std::borrow::Cow<'_, str> {
    if raw.contains('T') {
        std::borrow::Cow::Owned(crate::normalize_timestamp(raw))
    } else {
        std::borrow::Cow::Borrowed(raw)
    }
}

pub fn parse_log_line(line: &str) -> Option<LogEntry> {
    let rest = line.strip_prefix('[')?;
    let (timestamp, rest) = rest.split_once("] [")?;
    let (level, rest) = rest.split_once("] [")?;
    let (source, rest) = rest.split_once("] ")?;
    let timestamp = normalize_read_timestamp(timestamp);

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

/// 按筛选条件分页读取日志。
///
/// # 与 `read_log_entries_from_dir` 的区别
/// 后者把整天日志全读进 `Vec` 再返回，15 实例场景下会读出 GB 级文本。
/// 本函数**逐行流式处理**：每行 parse 后立即判定是否命中，不命中就丢弃，
/// 命中的也只在落在当前页区间时才留下。内存占用与「当页条数」成正比，
/// 与当天日志总量无关。
///
/// 为拿到 `total` 仍需扫完全部行（否则前端无法显示「共 X 条」、也无法
/// 知道有没有下一页），但扫描过程不累积数据，代价只是 IO 与 parse。
///
/// 插件名列表由**全部行**统计而来、不受筛选影响——否则按插件名筛过一次后，
/// 下拉框里就只剩当前选中的那个，用户无法再切回其它插件。
pub fn read_log_page_from_dir(dir: &Path, date: &str, q: &LogQuery) -> Result<LogPage, String> {
    use std::io::{BufRead, BufReader};

    let base_name = if q.error_only {
        build_error_log_filename(date)
    } else {
        build_log_filename(date)
    };

    let limit = normalize_limit(q.limit);
    let mut entries = Vec::new();
    let mut total = 0usize;
    let mut plugin_names = std::collections::BTreeSet::new();

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
        let file = match fs::File::open(&path) {
            Ok(f) => f,
            Err(_) => continue, // 文件不存在或读取失败：跳过而非整体报错
        };
        for line in BufReader::new(file).lines() {
            // 单行读取失败（如非 UTF-8 字节）跳过该行，不中断整体读取
            let line = match line {
                Ok(l) => l,
                Err(_) => continue,
            };
            let entry = match parse_log_line(&line) {
                Some(e) => e,
                None => continue,
            };
            plugin_names.insert(entry.plugin_name.clone());
            if !entry_matches(&entry, q) {
                continue;
            }
            // 命中计数照常累加，但只有落在当前页区间内的才真正留下
            if total >= q.offset && entries.len() < limit {
                entries.push(entry);
            }
            total += 1;
        }
    }

    Ok(LogPage {
        entries,
        total,
        plugin_names: plugin_names.into_iter().collect(),
    })
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

// ─────────────────────────────────────────────────────────────────
// 跨实例异常聚合（DEV-125123，供 AI 排查）
//
// 一台机器跑 3~15 个 Chrome 实例，日志全灌进同一份文件，一天可达几十万行。
// AI 直接读全量既慢又抓不住重点；真正能下判断的信息是「今天 ERROR 4 万条，
// 其中 robot-07 占 3.8 万、都是同一个 pattern」——故需要先聚合再给它看。
// ─────────────────────────────────────────────────────────────────

/// 单个实例的日志计数
#[derive(Debug, Clone, serde::Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PluginStat {
    pub plugin_name: String,
    /// 该实例的总条数
    pub total: usize,
    /// 其中 ERROR/WARN 条数——排序依据，异常多的实例排前面
    pub errors: usize,
}

/// 一类归并后的错误
#[derive(Debug, Clone, serde::Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ErrorCluster {
    /// 归一后的错误特征（可变部分已替换为占位符）
    pub pattern: String,
    pub count: usize,
    /// 出现过该错误的实例名——**跨实例关联的关键**：
    /// 「15 台里 3 台同时报同一个错」与「1 台报了 3 万次」是完全不同的问题
    pub plugins: Vec<String>,
    pub first_seen: String,
    pub last_seen: String,
    /// 一条未归一的原始消息，供人/AI 看具体长什么样
    pub sample: String,
}

/// 某天日志的聚合概览
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogSummary {
    pub date: String,
    /// 全部条数（不受任何筛选影响）
    pub total: usize,
    /// 各级别条数，键为大写级别名
    pub by_level: std::collections::BTreeMap<String, usize>,
    /// 各实例统计，异常多的排前
    pub by_plugin: Vec<PluginStat>,
    /// 归并后的错误类别，按出现次数倒序
    pub top_errors: Vec<ErrorCluster>,
}

/// `top_errors` 最多返回的类别数。取 20 是因为：聚合的用途是「一眼看出主要
/// 矛盾」，尾部长尾对判断没帮助；条数多时应改用筛选查明细而非扩大这个数
pub const MAX_ERROR_CLUSTERS: usize = 20;

/// 把错误消息归一成「特征」，供同类归并。
///
/// # 为什么必须归一
/// 「商品 12345 抓取失败」与「商品 67890 抓取失败」是同一个问题，但字面不同。
/// 不归一就会被算成两类，几万条各不相同的消息会产出几万个「类别」，聚合完全
/// 失去意义——这正是本函数存在的唯一理由。
///
/// 替换顺序有讲究：URL 必须在数字之前处理，否则 URL 里的数字会先被替换掉、
/// 导致同一个接口的不同参数被当成不同 URL。
pub fn normalize_error_pattern(message: &str) -> String {
    let mut out = String::with_capacity(message.len());
    let bytes = message.as_bytes();
    let mut i = 0;

    while i < message.len() {
        // URL：从 http 开始吃到空白为止，整段替换
        if message[i..].starts_with("http://") || message[i..].starts_with("https://") {
            out.push_str("<URL>");
            while i < message.len() && !bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            continue;
        }
        // 连续数字：2 位以上视为可变值（ID/数量/耗时）。
        // 单个数字保留——「第 1 步」这类序号是特征本身的一部分
        if bytes[i].is_ascii_digit() {
            let start = i;
            while i < message.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            if i - start >= 2 {
                out.push_str("<N>");
            } else {
                out.push_str(&message[start..i]);
            }
            continue;
        }
        // 其余字符按 UTF-8 字符边界原样保留（不能按字节推进，中文会被截断）
        let ch = message[i..].chars().next().unwrap_or('\u{FFFD}');
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// 流式聚合器。
///
/// # 为什么不能复用 `read_log_page_from_dir` 的结果做聚合
/// 那个函数受 `MAX_PAGE_LIMIT`（2000）约束，只返回一页。拿一页去聚合等于
/// 用 0.5% 的样本算「哪个实例异常最多」——排名完全不可信，而输出看起来
/// 却像正经统计，比不给更危险。
///
/// 聚合只累加计数与少量归并键，内存占用与「错误类别数」成正比而非日志量，
/// 故可以放心扫全量：40 万行实测 0.2 秒级。
#[derive(Default)]
struct SummaryAccumulator {
    by_level: std::collections::BTreeMap<String, usize>,
    plugin_total: std::collections::BTreeMap<String, usize>,
    plugin_errors: std::collections::BTreeMap<String, usize>,
    clusters: std::collections::BTreeMap<String, ErrorCluster>,
    total: usize,
}

impl SummaryAccumulator {
    fn push(&mut self, e: &LogEntry) {
        self.total += 1;
        let level = e.level.to_uppercase();
        *self.by_level.entry(level).or_insert(0) += 1;
        *self.plugin_total.entry(e.plugin_name.clone()).or_insert(0) += 1;

        if !crate::is_error_level(&e.level) {
            return;
        }
        *self.plugin_errors.entry(e.plugin_name.clone()).or_insert(0) += 1;

        let pattern = normalize_error_pattern(&e.message);
        match self.clusters.get_mut(&pattern) {
            Some(c) => {
                c.count += 1;
                if !c.plugins.contains(&e.plugin_name) {
                    c.plugins.push(e.plugin_name.clone());
                }
                // 按文件顺序（即时间顺序）遍历，故后来者即更晚
                c.last_seen = e.timestamp.clone();
            }
            None => {
                self.clusters.insert(
                    pattern.clone(),
                    ErrorCluster {
                        pattern,
                        count: 1,
                        plugins: vec![e.plugin_name.clone()],
                        first_seen: e.timestamp.clone(),
                        last_seen: e.timestamp.clone(),
                        sample: e.message.clone(),
                    },
                );
            }
        }
    }

    fn finish(self, date: &str) -> LogSummary {
        let mut by_plugin: Vec<PluginStat> = self
            .plugin_total
            .into_iter()
            .map(|(plugin_name, total)| {
                let errors = self.plugin_errors.get(&plugin_name).copied().unwrap_or(0);
                PluginStat {
                    plugin_name,
                    total,
                    errors,
                }
            })
            .collect();
        // 异常多的排前，其次总量，最后名字（保证顺序稳定可测）
        by_plugin.sort_by(|a, b| {
            b.errors
                .cmp(&a.errors)
                .then(b.total.cmp(&a.total))
                .then(a.plugin_name.cmp(&b.plugin_name))
        });

        let mut top_errors: Vec<ErrorCluster> = self.clusters.into_values().collect();
        top_errors.sort_by(|a, b| b.count.cmp(&a.count).then(a.pattern.cmp(&b.pattern)));
        top_errors.truncate(MAX_ERROR_CLUSTERS);
        for c in &mut top_errors {
            c.plugins.sort();
        }

        LogSummary {
            date: date.to_string(),
            total: self.total,
            by_level: self.by_level,
            by_plugin,
            top_errors,
        }
    }
}

/// 扫描某天全部日志（含切分文件）并聚合，**不受分页上限约束**。
///
/// 与 `read_log_page_from_dir` 一样逐行流式处理，不把条目累积进内存。
pub fn summarize_log_dir(dir: &Path, date: &str, error_only: bool) -> Result<LogSummary, String> {
    use std::io::{BufRead, BufReader};

    let base_name = if error_only {
        build_error_log_filename(date)
    } else {
        build_log_filename(date)
    };

    let mut acc = SummaryAccumulator::default();
    let mut candidates = vec![dir.join(&base_name)];
    for idx in 1..u32::MAX {
        let rotated = dir.join(build_rotated_filename(&base_name, idx));
        if !rotated.exists() {
            break;
        }
        candidates.push(rotated);
    }

    for path in candidates {
        let file = match fs::File::open(&path) {
            Ok(f) => f,
            Err(_) => continue,
        };
        for line in BufReader::new(file).lines() {
            let line = match line {
                Ok(l) => l,
                Err(_) => continue,
            };
            if let Some(entry) = parse_log_line(&line) {
                acc.push(&entry);
            }
        }
    }

    Ok(acc.finish(date))
}

/// 从内存中已有的条目聚合（供测试与小批量场景）。
///
/// 大批量请用 `summarize_log_dir`——它不需要先把全部条目读进内存。
pub fn summarize_entries(date: &str, entries: &[LogEntry]) -> LogSummary {
    let mut acc = SummaryAccumulator::default();
    for e in entries {
        acc.push(e);
    }
    acc.finish(date)
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
    fn test_parse_log_line_normalizes_utc_timestamp_on_read() {
        // 归一发生在写入时（log_server），所以老客户端落盘的行永远是 UTC。
        // 同一个文件里两种格式混着，时间段筛选按「第一个 HH:MM」取值当北京时间比，
        // 对 UTC 那批会整体偏 8 小时——2026-08-24 在 10 号机实测到这个现象。
        // 读取侧再归一一次，让存量数据也对齐，不必等 7 天保留期滚完。
        let entry = parse_log_line(
            "[2026-08-24T05:43:32.721Z] [WARN] [sidepanel] [10-1-LS10344] 超时",
        )
        .expect("UTC 时间戳的行应能解析");
        assert_eq!(
            entry.timestamp, "2026-08-24 13:43:32.721",
            "带 Z 的 UTC 时间戳应归一为北京时间，否则时间段筛选偏 8 小时"
        );
    }

    #[test]
    fn test_parse_log_line_keeps_local_timestamp_unchanged() {
        // 归一必须幂等：新客户端写入时已经归一过，读取时不能再加 8 小时
        let entry = parse_log_line(
            "[2026-08-24 13:51:27.176] [WARN] [sidepanel] [10-1-LS10344] 超时",
        )
        .expect("本地格式时间戳应能解析");
        assert_eq!(
            entry.timestamp, "2026-08-24 13:51:27.176",
            "已是本地格式的时间戳不可再次偏移，否则新写入的行会被推到未来"
        );
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

    /// 造一份含 n 条、插件名/级别交替的日志文件，用于分页与筛选测试
    fn write_sample_log(dir: &Path, date: &str, n: usize) {
        let mut content = String::new();
        for i in 0..n {
            let level = if i % 3 == 0 { "ERROR" } else { "INFO" };
            let plugin = if i % 2 == 0 { "robot-01" } else { "robot-02" };
            content.push_str(&format!(
                "[{} {:02}:{:02}:00.000] [{}] [background] [{}] 第{}条消息\n",
                date,
                i / 60,
                i % 60,
                level,
                plugin,
                i
            ));
        }
        fs::write(dir.join(build_log_filename(date)), content).expect("写入样例日志失败");
    }

    // ─────────────────────────────────────────────────────────
    // 跨实例异常聚合（DEV-125123）
    // ─────────────────────────────────────────────────────────

    fn entry(ts: &str, level: &str, plugin: &str, msg: &str) -> LogEntry {
        LogEntry {
            timestamp: ts.to_string(),
            level: level.to_string(),
            source: "background".to_string(),
            plugin_name: plugin.to_string(),
            message: msg.to_string(),
        }
    }

    #[test]
    fn test_normalize_replaces_multi_digit_numbers() {
        // 同一问题的不同 ID 必须归成一类，否则几万条各不相同的消息会产出
        // 几万个「类别」，聚合完全失去意义
        assert_eq!(
            normalize_error_pattern("商品 12345 抓取失败"),
            normalize_error_pattern("商品 67890 抓取失败"),
            "只有 ID 不同的同类错误必须归并"
        );
    }

    #[test]
    fn test_normalize_keeps_single_digit() {
        // 「第 1 步」这类序号是特征本身，不该被抹掉
        let a = normalize_error_pattern("第 1 步失败");
        let b = normalize_error_pattern("第 2 步失败");
        assert_ne!(a, b, "单个数字属于特征，不应归并");
    }

    #[test]
    fn test_normalize_replaces_urls_wholesale() {
        assert_eq!(
            normalize_error_pattern("请求 https://air.1688.com/x?id=99 超时"),
            normalize_error_pattern("请求 https://detail.1688.com/y?id=1 超时"),
            "URL 应整段替换，否则同一接口的不同参数会被当成不同错误"
        );
    }

    #[test]
    fn test_normalize_preserves_chinese() {
        // 按字节推进会截断中文，必须按字符边界处理
        let out = normalize_error_pattern("供应商未回复");
        assert_eq!(out, "供应商未回复", "中文必须原样保留，不能出现乱码");
    }

    #[test]
    fn test_summary_counts_by_level() {
        let entries = vec![
            entry("2026-08-22 10:00:00.000", "ERROR", "robot-01", "崩了"),
            entry("2026-08-22 10:00:01.000", "INFO", "robot-01", "正常"),
            entry("2026-08-22 10:00:02.000", "INFO", "robot-02", "正常"),
        ];
        let s = summarize_entries("2026-08-22", &entries);
        assert_eq!(s.total, 3);
        assert_eq!(s.by_level.get("ERROR"), Some(&1));
        assert_eq!(s.by_level.get("INFO"), Some(&2));
    }

    #[test]
    fn test_summary_ranks_instances_by_error_count() {
        // 「哪个实例在报错」是 AI 排查的第一个问题，异常多的必须排最前
        let mut entries = vec![entry("2026-08-22 10:00:00.000", "INFO", "robot-01", "正常")];
        for i in 0..5 {
            entries.push(entry(
                "2026-08-22 10:00:01.000",
                "ERROR",
                "robot-07",
                &format!("抓取失败 {}", i * 100),
            ));
        }
        let s = summarize_entries("2026-08-22", &entries);
        assert_eq!(
            s.by_plugin[0].plugin_name, "robot-07",
            "错误最多的实例必须排最前，实际: {:?}",
            s.by_plugin
        );
        assert_eq!(s.by_plugin[0].errors, 5);
        assert_eq!(s.by_plugin[1].errors, 0);
    }

    #[test]
    fn test_summary_clusters_same_error_across_instances() {
        // 「3 台同时报同一个错」与「1 台报了 3 次」是完全不同的问题，
        // plugins 字段就是为区分这两者存在的
        let entries = vec![
            entry("2026-08-22 10:00:00.000", "ERROR", "robot-01", "商品 111 抓取失败"),
            entry("2026-08-22 10:00:05.000", "ERROR", "robot-02", "商品 222 抓取失败"),
            entry("2026-08-22 10:00:09.000", "ERROR", "robot-03", "商品 333 抓取失败"),
        ];
        let s = summarize_entries("2026-08-22", &entries);
        assert_eq!(s.top_errors.len(), 1, "三条同类错误应归并为一类");
        let c = &s.top_errors[0];
        assert_eq!(c.count, 3);
        assert_eq!(
            c.plugins,
            vec!["robot-01", "robot-02", "robot-03"],
            "必须记录出现过该错误的全部实例"
        );
        assert_eq!(c.first_seen, "2026-08-22 10:00:00.000");
        assert_eq!(c.last_seen, "2026-08-22 10:00:09.000");
        assert!(c.sample.contains("111"), "sample 应保留一条未归一的原始消息");
    }

    #[test]
    fn test_summary_sorts_clusters_by_count() {
        let mut entries = vec![entry("2026-08-22 10:00:00.000", "ERROR", "robot-01", "少见错误")];
        for _ in 0..4 {
            entries.push(entry("2026-08-22 10:00:01.000", "ERROR", "robot-01", "常见错误"));
        }
        let s = summarize_entries("2026-08-22", &entries);
        assert_eq!(s.top_errors[0].pattern, "常见错误", "出现次数多的排前");
        assert_eq!(s.top_errors[0].count, 4);
    }

    #[test]
    fn test_summary_counts_warn_as_error() {
        // 与 is_error_level 口径一致：WARN 也进异常统计，否则「异常单独归集」
        // 的文件里有 WARN 而聚合里没有，两处对不上
        let entries = vec![entry("2026-08-22 10:00:00.000", "WARN", "robot-01", "重试中")];
        let s = summarize_entries("2026-08-22", &entries);
        assert_eq!(s.by_plugin[0].errors, 1, "WARN 应计入异常");
        assert_eq!(s.top_errors.len(), 1);
    }

    #[test]
    fn test_summarize_log_dir_reads_rotated_files() {
        // 切分文件必须一并统计，否则当天后半段日志的异常被漏掉
        let tmp = TempDir::new().expect("创建临时目录失败");
        let dir = tmp.path();
        let base = build_log_filename("2026-08-22");
        fs::write(
            dir.join(&base),
            "[2026-08-22 10:00:00.000] [ERROR] [background] [robot-01] 主文件错误 111\n",
        )
        .expect("写入失败");
        fs::write(
            dir.join(build_rotated_filename(&base, 1)),
            "[2026-08-22 11:00:00.000] [ERROR] [background] [robot-02] 主文件错误 222\n",
        )
        .expect("写入失败");

        let s = summarize_log_dir(dir, "2026-08-22", false).expect("聚合应成功");
        assert_eq!(s.total, 2, "切分文件必须一并统计");
        assert_eq!(s.top_errors.len(), 1, "跨文件的同类错误应归并");
        assert_eq!(s.top_errors[0].plugins, vec!["robot-01", "robot-02"]);
    }

    #[test]
    fn test_summarize_log_dir_missing_file_returns_empty() {
        let tmp = TempDir::new().expect("创建临时目录失败");
        let s = summarize_log_dir(tmp.path(), "2099-01-01", false)
            .expect("无日志应返回空聚合而非报错");
        assert_eq!(s.total, 0);
    }

    #[test]
    fn test_summary_of_empty_input_does_not_panic() {
        let s = summarize_entries("2026-08-22", &[]);
        assert_eq!(s.total, 0);
        assert!(s.by_plugin.is_empty());
        assert!(s.top_errors.is_empty());
        assert_eq!(s.date, "2026-08-22");
    }

    #[test]
    fn test_summary_caps_cluster_count() {
        // 长尾对判断没帮助；条数多时应改用筛选查明细而非扩大这个数
        let entries: Vec<LogEntry> = (0..MAX_ERROR_CLUSTERS + 10)
            .map(|i| {
                entry(
                    "2026-08-22 10:00:00.000",
                    "ERROR",
                    "robot-01",
                    &format!("错误类型{}", char::from(b'a' + (i % 26) as u8)),
                )
            })
            .collect();
        let s = summarize_entries("2026-08-22", &entries);
        assert!(
            s.top_errors.len() <= MAX_ERROR_CLUSTERS,
            "类别数必须有上限，实际 {}",
            s.top_errors.len()
        );
    }

    #[test]
    fn test_read_log_page_limits_returned_entries() {
        let tmp = TempDir::new().expect("创建临时目录失败");
        write_sample_log(tmp.path(), "2026-08-21", 1000);

        let q = LogQuery {
            limit: 100,
            ..Default::default()
        };
        let page = read_log_page_from_dir(tmp.path(), "2026-08-21", &q).expect("读取应成功");

        assert_eq!(
            page.entries.len(),
            100,
            "必须只返回一页的条数——一次性返回全量正是日志页卡顿的根因"
        );
        assert_eq!(
            page.total, 1000,
            "total 必须是命中筛选的全部条数，否则前端无法显示共多少条、也不知道有没有下一页"
        );
    }

    #[test]
    fn test_read_log_page_offset_skips_earlier_entries() {
        let tmp = TempDir::new().expect("创建临时目录失败");
        write_sample_log(tmp.path(), "2026-08-21", 100);

        let q = LogQuery {
            offset: 10,
            limit: 5,
            ..Default::default()
        };
        let page = read_log_page_from_dir(tmp.path(), "2026-08-21", &q).expect("读取应成功");

        assert_eq!(page.entries.len(), 5);
        assert!(
            page.entries[0].message.contains("第10条"),
            "offset=10 应从第 11 条开始，实际: {}",
            page.entries[0].message
        );
    }

    #[test]
    fn test_read_log_page_zero_limit_falls_back_to_default() {
        let tmp = TempDir::new().expect("创建临时目录失败");
        write_sample_log(tmp.path(), "2026-08-21", 1000);

        // 前端漏传 limit 时不能退回「全量返回」——那正是要修的问题
        let page = read_log_page_from_dir(tmp.path(), "2026-08-21", &LogQuery::default())
            .expect("读取应成功");

        assert_eq!(
            page.entries.len(),
            DEFAULT_PAGE_LIMIT,
            "limit 缺省必须回落到默认页大小，不能变成全量返回"
        );
    }

    #[test]
    fn test_read_log_page_limit_is_capped() {
        assert_eq!(
            normalize_limit(999_999),
            MAX_PAGE_LIMIT,
            "超大 limit 必须被截断，否则前端传个极大值就等于绕过分页"
        );
        assert_eq!(normalize_limit(0), DEFAULT_PAGE_LIMIT);
        assert_eq!(normalize_limit(50), 50);
    }

    #[test]
    fn test_read_log_page_filters_by_level_before_paging() {
        let tmp = TempDir::new().expect("创建临时目录失败");
        write_sample_log(tmp.path(), "2026-08-21", 300);

        let q = LogQuery {
            levels: vec!["ERROR".into()],
            limit: 1000,
            ..Default::default()
        };
        let page = read_log_page_from_dir(tmp.path(), "2026-08-21", &q).expect("读取应成功");

        assert_eq!(page.total, 100, "300 条里每 3 条一个 ERROR，应命中 100 条");
        assert!(
            page.entries.iter().all(|e| e.level == "ERROR"),
            "筛选必须在后端生效，不能把不命中的条目返回给前端再过滤"
        );
    }

    #[test]
    fn test_read_log_page_filters_by_plugin_name() {
        let tmp = TempDir::new().expect("创建临时目录失败");
        write_sample_log(tmp.path(), "2026-08-21", 100);

        let q = LogQuery {
            plugin_names: vec!["robot-02".into()],
            limit: 1000,
            ..Default::default()
        };
        let page = read_log_page_from_dir(tmp.path(), "2026-08-21", &q).expect("读取应成功");

        assert_eq!(page.total, 50);
        assert!(page.entries.iter().all(|e| e.plugin_name == "robot-02"));
    }

    #[test]
    fn test_read_log_page_keyword_is_case_insensitive() {
        let tmp = TempDir::new().expect("创建临时目录失败");
        let dir = tmp.path();
        fs::write(
            dir.join(build_log_filename("2026-08-21")),
            "[2026-08-21 10:00:00.000] [INFO] [background] [robot-01] Timeout occurred\n\
             [2026-08-21 10:00:01.000] [INFO] [background] [robot-01] 一切正常\n",
        )
        .expect("写入失败");

        let q = LogQuery {
            keyword: "TIMEOUT".into(),
            ..Default::default()
        };
        let page = read_log_page_from_dir(dir, "2026-08-21", &q).expect("读取应成功");

        assert_eq!(
            page.total, 1,
            "关键词应大小写不敏感——排查时不该因为大小写记错而搜不到"
        );
    }

    #[test]
    fn test_read_log_page_filters_by_time_range() {
        let tmp = TempDir::new().expect("创建临时目录失败");
        write_sample_log(tmp.path(), "2026-08-21", 180);

        // 时间段沿用原前端控件的 HH:MM 粒度
        let q = LogQuery {
            start_time: "01:00".into(),
            end_time: "01:59".into(),
            limit: 1000,
            ..Default::default()
        };
        let page = read_log_page_from_dir(tmp.path(), "2026-08-21", &q).expect("读取应成功");

        assert_eq!(page.total, 60, "应只命中 01 时段的 60 条");
        assert!(page
            .entries
            .iter()
            .all(|e| e.timestamp.starts_with("2026-08-21 01:")));
    }

    #[test]
    fn test_read_log_page_keyword_also_matches_source_and_plugin() {
        // 关键词匹配范围必须与改造前的前端过滤一致（消息+来源+级别+插件名），
        // 否则用户会觉得「搜索行为变了」
        let tmp = TempDir::new().expect("创建临时目录失败");
        let dir = tmp.path();
        fs::write(
            dir.join(build_log_filename("2026-08-21")),
            "[2026-08-21 10:00:00.000] [INFO] [sidepanel] [robot-01] 无关内容\n\
             [2026-08-21 10:00:01.000] [INFO] [background] [robot-02] 无关内容\n",
        )
        .expect("写入失败");

        let q = LogQuery {
            keyword: "sidepanel".into(),
            ..Default::default()
        };
        let page = read_log_page_from_dir(dir, "2026-08-21", &q).expect("读取应成功");
        assert_eq!(page.total, 1, "关键词应能匹配到来源字段，而非只匹配消息");

        let q2 = LogQuery {
            keyword: "robot-02".into(),
            ..Default::default()
        };
        let page2 = read_log_page_from_dir(dir, "2026-08-21", &q2).expect("读取应成功");
        assert_eq!(page2.total, 1, "关键词应能匹配到插件名");
    }

    #[test]
    fn test_read_log_page_level_filter_is_case_insensitive() {
        // 日志文件里级别可能大小写混用，筛选不该因此漏掉条目
        let tmp = TempDir::new().expect("创建临时目录失败");
        let dir = tmp.path();
        fs::write(
            dir.join(build_log_filename("2026-08-21")),
            "[2026-08-21 10:00:00.000] [error] [background] [robot-01] 小写级别\n\
             [2026-08-21 10:00:01.000] [ERROR] [background] [robot-01] 大写级别\n",
        )
        .expect("写入失败");

        let q = LogQuery {
            levels: vec!["ERROR".into()],
            ..Default::default()
        };
        let page = read_log_page_from_dir(dir, "2026-08-21", &q).expect("读取应成功");
        assert_eq!(page.total, 2, "级别筛选应大小写不敏感");
    }

    #[test]
    fn test_read_log_page_plugin_names_not_narrowed_by_filter() {
        let tmp = TempDir::new().expect("创建临时目录失败");
        write_sample_log(tmp.path(), "2026-08-21", 100);

        let q = LogQuery {
            plugin_names: vec!["robot-01".into()],
            ..Default::default()
        };
        let page = read_log_page_from_dir(tmp.path(), "2026-08-21", &q).expect("读取应成功");

        assert_eq!(
            page.plugin_names,
            vec!["robot-01".to_string(), "robot-02".to_string()],
            "插件名下拉选项必须来自全部日志、不受当前筛选影响，否则筛过一次就再也切不回其它插件"
        );
    }

    #[test]
    fn test_read_log_page_reads_rotated_files_in_order() {
        let tmp = TempDir::new().expect("创建临时目录失败");
        let dir = tmp.path();
        let base = build_log_filename("2026-08-21");
        fs::write(
            dir.join(&base),
            "[2026-08-21 10:00:00.000] [INFO] [background] [robot-01] 主文件\n",
        )
        .expect("写入失败");
        fs::write(
            dir.join(build_rotated_filename(&base, 1)),
            "[2026-08-21 11:00:00.000] [INFO] [background] [robot-01] 切分文件\n",
        )
        .expect("写入失败");

        let page = read_log_page_from_dir(dir, "2026-08-21", &LogQuery::default())
            .expect("读取应成功");

        assert_eq!(page.total, 2, "切分文件必须一并读取，否则会漏掉当天后半段日志");
        assert!(page.entries[0].message.contains("主文件"));
        assert!(page.entries[1].message.contains("切分文件"));
    }

    #[test]
    fn test_read_log_page_missing_file_returns_empty_not_error() {
        let tmp = TempDir::new().expect("创建临时目录失败");
        let page = read_log_page_from_dir(tmp.path(), "2099-01-01", &LogQuery::default())
            .expect("文件不存在时应返回空页而非报错");
        assert!(page.entries.is_empty());
        assert_eq!(page.total, 0);
    }

    #[test]
    fn test_read_log_page_error_only_reads_error_file() {
        let tmp = TempDir::new().expect("创建临时目录失败");
        let dir = tmp.path();
        fs::write(
            dir.join(build_log_filename("2026-08-21")),
            "[2026-08-21 10:00:00.000] [INFO] [background] [robot-01] 正常\n",
        )
        .expect("写入失败");
        fs::write(
            dir.join(build_error_log_filename("2026-08-21")),
            "[2026-08-21 10:00:01.000] [ERROR] [content] [robot-01] 出错了\n",
        )
        .expect("写入失败");

        let q = LogQuery {
            error_only: true,
            ..Default::default()
        };
        let page = read_log_page_from_dir(dir, "2026-08-21", &q).expect("读取应成功");

        assert_eq!(page.total, 1, "error_only 应只读异常文件");
        assert_eq!(page.entries[0].message, "出错了");
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
