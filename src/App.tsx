import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { getVersion } from "@tauri-apps/api/app";
import { listen } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import { check } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";
import "./App.css";

type Env = "online" | "test";

interface UpdateInfo {
  install_path: string;
  current_version: string;
  env: string;
  download_url: string;
}

interface CheckResult {
  has_update: boolean;
  current_version: string;
  remote_version: string;
  install_path: string;
}

interface LogEntry {
  timestamp: string;
  level: string;
  source: string;
  plugin_name: string;
  message: string;
}

/// 单个插件实例的巡检快照（DEV-125034）
interface PluginSnapshot {
  pluginName: string;
  silenceSecs: number;
  stale: boolean;
  sidepanelOpen: boolean;
  taskRunning: boolean;
  wsConnected: boolean | null;
  login1688: boolean | null;
  expectedAccount: string | null;
  actualAccount: string | null;
  accountMismatch: boolean;
  /** 当前登录的 CJ 账号。1688 账号说「登的哪个供应商侧账号」，它说「这台归谁用」 */
  cjAccount: string | null;
  hasIssue: boolean;
}

interface PatrolReport {
  instances: PluginSnapshot[];
  chromeWindows: number;
  minimizedWindows: number;
}

/// 巡检看板刷新间隔。心跳每 5 秒一次，看板同频即可
const PATROL_REFRESH_MS = 5000;

/// 后端分页查询的返回（DEV-122550）。total 是命中筛选的总条数，
/// 与 entries.length 不同——后者只是当前这一页
interface LogPage {
  entries: LogEntry[];
  total: number;
  plugin_names: string[];
}

/// 单页拉取条数。与后端 DEFAULT_PAGE_LIMIT 一致；日志表格一屏撑不过百行，
/// 500 条足够翻阅，再多只是白付 IPC 序列化与 DOM 渲染成本
const LOG_PAGE_SIZE = 500;

interface SystemSnapshot {
  total_memory_bytes: number;
  available_memory_bytes: number;
  cpu_brand: string;
  cpu_cores: number;
  cpu_usage_percent: number;
  disk_total_bytes: number;
  disk_available_bytes: number;
  os_version: string;
}

type View = "update" | "logs" | "machine" | "patrol" | "chrome-mapping";

/// Chrome Profile 映射表确认界面用的候选数据（DEV-125986）。
/// 字段命名对齐后端 ChromeProfileCandidates（camelCase）
interface ChromeProfileCandidates {
  directoryNames: string[];
  onlinePluginNames: string[];
  savedMapping: Record<string, string>;
}

/// 机器状态页的刷新间隔：3 秒。CPU 占用率类数据需要能看出实时变化，
/// 但采购同事的机器同时还在跑插件，间隔太短会增加不必要的采样开销
const MACHINE_STATUS_REFRESH_MS = 3000;

/** 字节数格式化为 GB，保留 1 位小数，供机器状态页展示 */
function formatBytesAsGb(bytes: number): string {
  return (bytes / 1024 / 1024 / 1024).toFixed(1) + " GB";
}

/// 判定机器是否处于高负载：CPU 占用 > 80% 或可用内存占比 < 10%。
/// 满足任一条件即预警——两者都是"快卡顿"的经验阈值，不要求同时满足
function isHighLoad(snapshot: SystemSnapshot): boolean {
  const memoryAvailableRatio =
    snapshot.total_memory_bytes === 0
      ? 1
      : snapshot.available_memory_bytes / snapshot.total_memory_bytes;
  return snapshot.cpu_usage_percent > 80 || memoryAvailableRatio < 0.1;
}

const ALL_LEVELS = ["ERROR", "WARN", "INFO", "DEBUG"];

/// 自更新检查间隔：4 小时。采购机器常整天开着，间隔太短徒增站点请求，
/// 太长则当天发的修复版当天到不了。
const SELF_UPDATE_INTERVAL_MS = 4 * 60 * 60 * 1000;

/// 轮询是否已启动。模块级而非组件 state：StrictMode 下 effect 跑两遍，
/// 用 state 无法在第二次执行时读到第一次的结果（闭包捕获的是旧值）
let selfUpdateStarted = false;

/// 自更新请求统一带的 Accept 头。
///
/// # 为什么必须显式指定
/// tauri-plugin-updater 在两个请求上填不同的默认值（updater.rs:389 / :659）：
/// check 用 `application/json`、download 用 `application/octet-stream`。
/// 而 chainai 站点的边缘 nginx 对 Accept 不含 `*/*`、也不像 json 的请求
/// **整站返回 500** —— 于是表现成「检查更新成功、下载必失败」，十几台采购机
/// 的自动升级从此全瘫（2026-08-24 实测：那天每 4 小时报一次 500，无一台升上来）。
///
/// 插件源码是 `if !headers.contains_key(ACCEPT)`，我们自己传就能覆盖默认值。
/// 用 `*/*` 而不是别的：实测该站点只放行含 `*/*` 或 `application/json` 的请求，
/// 而 `*/*` 语义上就是「什么都收」，对下载二进制完全正确。
const SELF_UPDATE_HEADERS = { Accept: "*/*" };

/// 同一条自更新错误的最短重记间隔：1 小时。
///
/// # 为什么按时间窗，而不是「只记一次」
/// 原先用 `lastSelfUpdateError` 做「同一错误只记一次、直到某次 check 成功才解除」。
/// 那在**持续失败**下会永久静默：断网的机器 check 每次都失败、永远等不到解除，
/// 于是第一条之后再也不记。排查者看到日志里没有异常，会误判成「这台没问题」——
/// 而这恰是最需要发现的状态。按时间窗则保证「一直坏就一直有记录」。
const SELF_UPDATE_ERROR_WINDOW_MS = 60 * 60 * 1000;

/// 自更新错误 → 上次记录时刻。配合上面的时间窗做抑制
const selfUpdateErrorSeen = new Map<string, number>();

function shouldLogSelfUpdateError(key: string): boolean {
  const now = Date.now();
  const prev = selfUpdateErrorSeen.get(key);
  if (prev !== undefined && now - prev < SELF_UPDATE_ERROR_WINDOW_MS) return false;
  selfUpdateErrorSeen.set(key, now);
  return true;
}

/// 从 latest.json 原文里取出本平台的下载地址。
///
/// 插件的 JS 类型没有暴露 downloadUrl（见 @tauri-apps/plugin-updater 的
/// UpdateMetadata），但 rawJson 是完整清单，自己取即可。失败日志里必须带地址：
/// 指错环境（测试包发到线上机）时靠它才能一眼看出来。
function resolveDownloadUrl(rawJson: Record<string, unknown>): string {
  const platforms = (rawJson?.platforms ?? {}) as Record<
    string,
    { url?: string }
  >;
  const osPrefix = navigator.userAgent.includes("Windows")
    ? "windows"
    : navigator.userAgent.includes("Mac")
      ? "darwin"
      : "";
  const key = Object.keys(platforms).find((k) => k.startsWith(osPrefix));
  return (key && platforms[key]?.url) || "（清单未给出本平台地址）";
}

/// 更新器自身的更新状态
type SelfUpdateState =
  | { kind: "idle" }
  | { kind: "downloading"; version: string }
  | { kind: "ready"; version: string; notes: string };

function App() {
  // 默认打开巡检页：日常最先要看的就是「哪台插件有问题」，
  // 更新管理是偶尔才用一次的功能
  const [view, setView] = useState<View>("patrol");
  const [activeEnv, setActiveEnv] = useState<Env>("online");
  const [appInfo, setAppInfo] = useState<UpdateInfo | null>(null);
  const [status, setStatus] = useState<string>("");
  const [loading, setLoading] = useState<boolean>(false);
  const [showConfirm, setShowConfirm] = useState<boolean>(false);
  const [checkResult, setCheckResult] = useState<CheckResult | null>(null);
  // 路径编辑状态
  const [editingPath, setEditingPath] = useState<boolean>(false);
  const [customPathInput, setCustomPathInput] = useState<string>("");
  // 日志查看状态
  const [logDates, setLogDates] = useState<string[]>([]);
  const [selectedDate, setSelectedDate] = useState<string>("");
  const [selectedLevels, setSelectedLevels] = useState<Set<string>>(new Set(ALL_LEVELS));
  const [availablePluginNames, setAvailablePluginNames] = useState<string[]>([]);
  const [selectedPluginNames, setSelectedPluginNames] = useState<Set<string>>(new Set());
  const [timeFrom, setTimeFrom] = useState<string>("");
  const [timeTo, setTimeTo] = useState<string>("");
  const [keyword, setKeyword] = useState<string>("");
  const [logEntries, setLogEntries] = useState<LogEntry[]>([]);
  const [logLoading, setLogLoading] = useState<boolean>(false);
  const [logError, setLogError] = useState<string>("");
  // 命中筛选的总条数（后端给出，可能远大于已加载的 logEntries.length）
  const [logTotal, setLogTotal] = useState<number>(0);
  // 已加载的页数，用于「加载更多」计算 offset
  const [logLoadedPages, setLogLoadedPages] = useState<number>(1);
  // 筛选条件的防抖版本：关键词逐字输入不该每个字符都打一次后端
  const [debouncedKeyword, setDebouncedKeyword] = useState<string>("");
  // 巡检看板（DEV-125034）
  const [patrol, setPatrol] = useState<PatrolReport | null>(null);
  const [patrolHint, setPatrolHint] = useState<string>("");
  // 正在下发的指令（`实例名:指令`），非 null 时禁用全部按钮。
  // 一次只允许一条在途：这些操作会改变插件状态，连点或并发下发时人无法
  // 判断哪条生效了——而巡检页每 5 秒刷新一次，结果很快就能看到
  const [sendingCmd, setSendingCmd] = useState<string | null>(null);

  // 机器状态：CPU/内存/磁盘/系统版本，辅助判断虚拟机是否卡顿
  const [systemSnapshot, setSystemSnapshot] = useState<SystemSnapshot | null>(null);

  // Chrome Profile 映射表确认（DEV-125986，独立进程架构专用）
  const [chromeCandidates, setChromeCandidates] = useState<ChromeProfileCandidates | null>(null);
  // 用户在界面上正在编辑的选择：目录名 -> 选中的 plugin_name（空字符串表示未选）。
  // 打开页面时用 savedMapping 预填，避免每次都要重新选一遍
  const [mappingDraft, setMappingDraft] = useState<Record<string, string>>({});
  const [mappingHint, setMappingHint] = useState<string>("");
  const [mappingSaving, setMappingSaving] = useState<boolean>(false);

  // 更新器自身的自动更新（区别于上面「更新 aichat 插件」的业务逻辑）
  const [selfUpdate, setSelfUpdate] = useState<SelfUpdateState>({ kind: "idle" });
  // 当前程序版本，显示在界面上——此前只能翻日志确认，排查十几台机器时很不方便
  const [appVersion, setAppVersion] = useState<string>("");
  // 手动检查更新的状态与反馈（自动轮询不产生这些提示）
  const [checkingUpdate, setCheckingUpdate] = useState<boolean>(false);
  const [selfUpdateHint, setSelfUpdateHint] = useState<string>("");
  // 自动检查更新是否开启（默认关）。关闭时巡检页顶部显示提示——
  // 「关了忘记开」是这类开关的典型问题，而静默不升级最难察觉
  // （2026-08-24 刚踩过自动更新坏了 6 天没人发现）
  const [autoUpdate, setAutoUpdate] = useState<boolean>(false);

  useEffect(() => {
    getVersion().then(setAppVersion).catch(() => {});
  }, []);

  // 托盘「立即检查更新」走事件通知：检查逻辑在前端，托盘只负责触发。
  // 与下面的轮询分开注册——轮询有防重入标志、注册一次即可，
  // 而监听必须每次挂载都注册，否则 StrictMode 第二次执行时会漏挂
  useEffect(() => {
    const unlisten = listen("tray://check-update", () => runSelfUpdateCheck("manual"));
    return () => {
      unlisten.then((f) => f()).catch(() => {});
    };
  }, []);

  // 启动时检查一次，之后每 4 小时一次。下载在后台完成，
  // 但不自动重启——用户可能正在跑插件更新，中途重启会打断操作，
  // 改为下载完提示、由用户点「立即重启」或下次启动时自然生效。
  useEffect(() => {
    // StrictMode 下 effect 会执行两遍。仅靠 cancelled 标志能挡住状态更新，
    // 但挡不住已经发出的网络请求与日志——实测每次启动会重复记两条。
    // 用模块级标志保证同一进程内只启动一套轮询。
    if (selfUpdateStarted) return;
    selfUpdateStarted = true;

    let timer: ReturnType<typeof setInterval> | null = null;

    // 自动检查更新默认关（2026-08-27）。开关在托盘菜单，存 config.json。
    // 关掉时连启动那次检查也不做——测试期间不希望机器在背后升级、
    // 重启客户端打断测试，而启动检查恰好发生在最容易被忽略的时刻
    const start = (enabled: boolean) => {
      setAutoUpdate(enabled);
      if (timer) {
        clearInterval(timer);
        timer = null;
      }
      if (!enabled) return;
      runSelfUpdateCheck("auto");
      timer = setInterval(() => runSelfUpdateCheck("auto"), SELF_UPDATE_INTERVAL_MS);
    };

    invoke<boolean>("get_auto_update")
      .then(start)
      // 读不到偏好时按「关」处理：宁可不升级也不要在用户以为关着的时候偷偷升
      .catch(() => start(false));

    // 托盘里改了开关立即生效，不必重启客户端
    const un = listen<boolean>("auto-update-changed", (e) => start(e.payload));
    return () => {
      if (timer) clearInterval(timer);
      void un.then((f) => f());
    };
  }, []);

  /// 执行一次自更新检查。
  /// trigger 为 "manual" 时会把结果反馈到界面（采购同事点了按钮需要看到回应），
  /// "auto" 时保持安静——后台轮询不应打扰正在干活的人。
  async function runSelfUpdateCheck(trigger: "auto" | "manual") {
    if (checkingUpdate) return; // 防止连点或与轮询重入
    setCheckingUpdate(true);
    if (trigger === "manual") setSelfUpdateHint("正在检查更新…");
    const startedAt = Date.now();
    // 下载地址在失败日志里是必备字段，但只有拿到 update 才知道。
    // 提前声明，让 catch 分支也能用上（check 阶段就失败时保持「未知」）
    let downloadUrl = "（未取到清单）";
    let stage = "检查";
    try {
      const update = await check({ headers: SELF_UPDATE_HEADERS });
      if (!update) {
        // **成功也记**：原先只在失败时写日志，于是「这台机器还在检查更新吗」
        // 根本判断不了 —— 定时器死了、进程僵住、一切正常，在日志里长得一模一样
        // （都是没有日志）。每 4 小时一条的节律本身就是客户端存活心跳。
        // 直接问 getVersion() 而不用 appVersion state：后者由另一个 effect
        // 异步填充，而自动检查在挂载时立即跑一次——首次必然读到空串，
        // 于是每次启动的第一条日志都会记成「当前=未知」，恰好是最想知道版本的时刻
        const current = await getVersion().catch(() => "未知");
        invoke("log_self_update_check", {
          trigger,
          current,
          remote: current, // check() 返回 null ⇒ 远端不比本地新
          verdict: "已是最新",
          elapsedMs: Date.now() - startedAt,
        }).catch(() => {});
        if (trigger === "manual") setSelfUpdateHint("已是最新版本");
        return;
      }
      downloadUrl = resolveDownloadUrl(update.rawJson);
      invoke("log_self_update_check", {
        trigger,
        current: update.currentVersion,
        remote: update.version,
        verdict: "发现新版",
        elapsedMs: Date.now() - startedAt,
      }).catch(() => {});
      setSelfUpdateHint("");
      stage = "下载";
      // 拆成 download + install 两步而非 downloadAndInstall()：后者失败时
      // 无法判断卡在下载还是安装，而这两者的成因完全不同（网络 vs 权限/占用）
      let received = 0;
      let total = 0;
      let nextMilestone = 25;
      const downloadStartedAt = Date.now();
      await update.download(
        (ev) => {
          if (ev.event === "Started") {
            total = ev.data.contentLength ?? 0;
            // 状态切到「正在下载」推迟到**确认服务端开始回数据之后**。
            // 原先在 download() 之前就切，于是服务端直接拒绝（如 500）时，
            // 界面会先显示「正在后台下载…」再跳出报错——看起来像下到一半坏了，
            // 实际一个字节都没下，把排查方向带偏
            setSelfUpdate({ kind: "downloading", version: update.version });
          } else if (ev.event === "Progress") {
            received += ev.data.chunkLength;
            // 按里程碑记而不是每个 chunk 记：4MB 的包有上千个 chunk。
            // 有了进度才能区分「卡住」和「失败」——卡住时界面停在下载中、
            // 日志里既无失败也无完成，是最难现场排查的一种状态
            if (total > 0 && (received * 100) / total >= nextMilestone) {
              invoke("log_self_update_progress", {
                received,
                total,
                elapsedMs: Date.now() - downloadStartedAt,
              }).catch(() => {});
              nextMilestone += 25;
            }
          }
        },
        { headers: SELF_UPDATE_HEADERS }
      );
      stage = "安装";
      // install() 之后进程即重启，「安装成功」这条永远写不进去。
      // 记下「即将退出安装」，配上下次启动的「客户端启动，版本 X（升级前 Y）」，
      // 安装到底有没有发生就闭环了
      invoke("log_self_update_info", {
        message: `下载完成 ${received}/${total} 字节，即将退出并安装 ${update.version}`,
      }).catch(() => {});
      await update.install();
      // body 来自 latest.json 的 notes 字段，用于告知这次更新改了什么；
      // 缺失时不显示说明，但不影响升级本身
      setSelfUpdate({
        kind: "ready",
        version: update.version,
        notes: (update.body ?? "").trim(),
      });
    } catch (e) {
      const msg = String(e);
      const elapsedMs = Date.now() - startedAt;
      // 失败必须把界面状态收回来。否则提示条会永远停在「正在后台下载…」——
      // 既不报错也不消失，比直接说失败更让人困惑
      setSelfUpdate({ kind: "idle" });

      // 当前平台不在清单里属正常情况（只发布 Windows 包），不是故障。
      //
      // 这个判断必须放在 catch 里：check() 内部在**版本比较之前**就调
      // get_urls()?（updater.rs:534），清单没有本平台的键时它直接抛错 ——
      // 哪怕根本不需要更新。原先在 check() 之后做的平台预检是死代码，
      // 走不到，于是 macOS 上点「检查更新」看到的是一句生英文报错
      const isPlatformMissing = msg.includes("were found in the response");
      if (isPlatformMissing) {
        if (trigger === "manual") setSelfUpdateHint("当前系统暂无更新包");
        // 记成 INFO 而非 ERROR（不污染「仅异常」视图），但**必须记** ——
        // 不记就等于留了一条静默路径：这台机器每 4 小时走一次这里、日志里
        // 一个字都没有，与「定时器死了」无法区分。而「看到没有日志就以为没问题」
        // 正是 2026-08-24 那次栽跟头的方式
        if (shouldLogSelfUpdateError(msg)) {
          invoke("log_self_update_check", {
            trigger,
            current: await getVersion().catch(() => "未知"),
            remote: "未知",
            verdict: "清单无本平台包（只发布 Windows 包时属正常）",
            elapsedMs,
          }).catch(() => {});
        }
        return;
      }

      // 报错文案要让非技术人员看得懂：采购同事看到
      // 「Download request failed with status: 500 Internal Server Error」
      // 既不懂、也不知道该找谁。原因照旧完整进日志，界面只给能行动的一句话
      if (trigger === "manual") {
        setSelfUpdateHint(
          `更新失败（${stage}阶段），已记入日志，请联系 IT。`
        );
      }

      if (shouldLogSelfUpdateError(msg)) {
        invoke("log_self_update_failure", {
          stage,
          url: downloadUrl,
          detail: `详情=${msg}`,
          elapsedMs,
        }).catch(() => {});
        // 失败后对同一地址做一次带/不带 Accept 的对比探测，让日志自己说出
        // 根因类别。2026-08-24 的 500 是人手动 curl 对比才定位的，
        // 中间怀疑过网络、CDN、文件损坏——那些弯路这条探针能省掉
        if (stage === "下载" && downloadUrl.startsWith("http")) {
          invoke("diagnose_download_url", { url: downloadUrl }).catch(() => {});
        }
      }
      console.error("自动更新检查失败:", e);
    } finally {
      setCheckingUpdate(false);
    }
  }

  useEffect(() => {
    setStatus("");
    setShowConfirm(false);
    setCheckResult(null);
    setAppInfo(null);
    setEditingPath(false);
    loadAppInfo(activeEnv);
  }, [activeEnv]);

  // 切到日志页时加载可选日期列表，默认选最新一天
  useEffect(() => {
    if (view !== "logs") return;
    invoke<string[]>("list_log_dates").then((dates) => {
      setLogDates(dates);
      setSelectedDate((prev) => prev || dates[0] || "");
    });
  }, [view]);

  // 关键词防抖：逐字输入不该每敲一个字符就打一次后端
  useEffect(() => {
    const timer = setTimeout(() => setDebouncedKeyword(keyword), 300);
    return () => clearTimeout(timer);
  }, [keyword]);

  // 日期或筛选条件变化时，回到第一页重新查
  useEffect(() => {
    setLogLoadedPages(1);
  }, [selectedDate, selectedLevels, selectedPluginNames, timeFrom, timeTo, debouncedKeyword]);

  // 读取当前页日志。筛选与分页都下推到后端（DEV-122550）——一台机器跑 3~15 个
  // Chrome 实例，当天日志可达 GB 级，原先「全量拉取 + 前端内存过滤 + 全量渲染」
  // 会在读文件、IPC 序列化、DOM 渲染三处各卡一次。
  //
  // 筛选不能只在前端已加载的那一页里做：日志页是用来排查问题的，筛选必须覆盖
  // 当天全部数据，否则等于筛不到。
  useEffect(() => {
    if (view !== "logs" || !selectedDate) return;
    setLogLoading(true);
    setLogError("");
    // 级别/插件名全选时传空数组表示「不限」，避免把整份白名单塞进 IPC
    const levels =
      selectedLevels.size === ALL_LEVELS.length ? [] : Array.from(selectedLevels);
    const pluginNames =
      availablePluginNames.length > 0 &&
      selectedPluginNames.size === availablePluginNames.length
        ? []
        : Array.from(selectedPluginNames);
    invoke<LogPage>("read_log_page", {
      date: selectedDate,
      query: {
        errorOnly: false,
        levels,
        pluginNames,
        keyword: debouncedKeyword,
        startTime: timeFrom,
        endTime: timeTo,
        offset: 0,
        limit: LOG_PAGE_SIZE * logLoadedPages,
      },
    })
      .then((result) => {
        setLogEntries(result.entries);
        setLogTotal(result.total);
        setAvailablePluginNames((prev) => {
          // 插件名下拉选项来自全部日志、不受当前筛选影响。仅在切换日期
          // （选项集合真的变了）时重置勾选，否则每次筛选都会把勾选冲掉
          const changed =
            prev.length !== result.plugin_names.length ||
            prev.some((p, i) => p !== result.plugin_names[i]);
          if (changed) setSelectedPluginNames(new Set(result.plugin_names));
          return changed ? result.plugin_names : prev;
        });
      })
      .catch((e) => setLogError(`读取日志失败: ${e}`))
      .finally(() => setLogLoading(false));
  }, [
    view,
    selectedDate,
    selectedLevels,
    selectedPluginNames,
    timeFrom,
    timeTo,
    debouncedKeyword,
    logLoadedPages,
  ]);

  // 巡检看板：仅在该页时轮询，避免后台白跑
  useEffect(() => {
    if (view !== "patrol") return;
    const refresh = () => {
      invoke<PatrolReport>("get_patrol_report")
        .then(setPatrol)
        .catch((e) => setPatrolHint(`读取巡检数据失败: ${e}`));
    };
    refresh();
    const timer = setInterval(refresh, PATROL_REFRESH_MS);
    return () => clearInterval(timer);
  }, [view]);

  /// 加载 Chrome Profile 映射表确认页的候选数据（DEV-125986）。
  ///
  /// 改成人工点按钮触发，而非进入页面/切 tab 自动拉取：早期版本用
  /// `useEffect([view])` 自动加载，副作用是——用户选了几行还没点保存，
  /// 切到别的 tab 再切回来，`useEffect` 会重新拉取并用 `savedMapping`
  /// 覆盖掉正在编辑但未保存的 `mappingDraft`，选择白做。改成手动触发后，
  /// 只有用户主动点"刷新候选"才会重新拉取，不会有任何意外覆盖。
  function loadChromeProfileCandidates() {
    setMappingHint("正在检测 Chrome 进程与在线实例…");
    invoke<ChromeProfileCandidates>("get_chrome_profile_candidates")
      .then((c) => {
        setChromeCandidates(c);
        setMappingDraft(c.savedMapping);
        setMappingHint("");
      })
      .catch((e) => setMappingHint(`读取候选数据失败: ${e}`));
  }

  /// 保存人工在映射表确认界面里选定的对应关系。
  ///
  /// 提交前过滤掉未选择的行（空字符串）——避免把"用户还没决定"误存成
  /// 一条无意义的空映射，那样下次查询时会因为找不到对应进程而误判成
  /// "该实例 Chrome 未启动"。
  async function saveChromeProfileMapping() {
    setMappingSaving(true);
    setMappingHint("正在保存…");
    try {
      const mapping: Record<string, string> = {};
      for (const [dir, pluginName] of Object.entries(mappingDraft)) {
        if (pluginName) mapping[dir] = pluginName;
      }
      const msg = await invoke<string>("save_chrome_profile_mapping_cmd", { mapping });
      setMappingHint(msg);
    } catch (e) {
      setMappingHint(`保存失败：${e}`);
    } finally {
      setMappingSaving(false);
    }
  }

  /// 点 Chrome 工具栏上的插件图标，打开指定实例的侧边栏
  /// （领导给的方案，走图像识别 + 模拟鼠标；DEV-125986 起按实例精确定位）。
  ///
  /// # 为什么要二次确认
  /// 它和那四个指令按钮性质完全不同：那些是通过心跳通道发给某个实例、
  /// 互不影响；这个会**把 Chrome 抢到最前并移动真实鼠标**，一台机器上
  /// 跑着 8~10 个实例、插件正往供应商聊天框打字时，按键会落到别处或丢字。
  ///
  /// 所以点之前要让人确认「现在这台机器可以被打断」。
  ///
  /// # 精确定位（DEV-125986）
  /// 后端按 `pluginName` 定位到具体窗口做区域限定点击，不再无差别全屏
  /// 找图标——两条定位路径（映射表/UI Automation）都失败时，后端仍会
  /// 自动回退到全屏扫描兜底，前端不需要额外的"全屏兜底"入口（原顶部
  /// 常驻按钮已移除，见 DEV-125986：表格行内按钮已覆盖唯一有效场景）。
  async function clickPluginIcon(pluginName: string) {
    const ok = window.confirm(
      "点插件图标会做两件有副作用的事：\n\n" +
        "1. 把 Chrome 窗口抢到最前（夺走焦点）\n" +
        "2. 移动真实鼠标指针并点击\n\n" +
        "如果这台机器上有实例正在往供应商聊天框输入文字，会被打断、\n" +
        "按键可能落到别处。确认现在可以打断吗？",
    );
    if (!ok) return;
    setSendingCmd(`icon:${pluginName}`);
    setPatrolHint(`正在识别并点击 ${pluginName} 的插件图标…（约需 1~2 秒）`);
    try {
      const msg = await invoke<string>("click_plugin_icon", { pluginName });
      setPatrolHint(msg);
    } catch (e) {
      // 失败原因对排查有用（没找到图标 = 可能要重截模板；没装 Python 等），
      // 原样显示，不要吞成一句「操作失败」
      setPatrolHint(`点击失败：${e}`);
    } finally {
      setSendingCmd(null);
    }
  }

  /// 向某个插件实例下发一条指令。
  ///
  /// # 提示语为什么强调「已下发」而不是「已完成」
  /// 指令入队后要等插件**下次心跳**才被取走（最长 5 秒），执行成功才回 ack。
  /// 说成「已完成」会让人以为立刻生效了，而实际插件可能压根没接上——
  /// 真正的结果看巡检表格自己的变化（每 5 秒刷新）。
  async function sendCommand(pluginName: string, kind: string) {
    const key = `${pluginName}:${kind}`;
    setSendingCmd(key);
    setPatrolHint(`正在向 ${pluginName} 下发 ${kind}…`);
    try {
      const msg = await invoke<string>("send_plugin_command", {
        pluginName,
        kind,
      });
      setPatrolHint(`${pluginName}: ${msg}`);
    } catch (e) {
      // 失败原因对使用者是有意义的（实例没上报过 = 插件没运行或版本过旧），
      // 不要吞掉换成一句「操作失败」
      setPatrolHint(`${pluginName} 下发失败: ${e}`);
    } finally {
      setSendingCmd(null);
    }
  }



  // 机器状态：常驻定时刷新（不只在「机器状态」页才采样），
  // 供标题栏简化指示器随时显示，无需切到该页才能看到负载情况
  useEffect(() => {
    const refresh = () => {
      invoke<SystemSnapshot>("get_system_snapshot", { env: activeEnv })
        .then(setSystemSnapshot)
        .catch(() => {});
    };
    refresh();
    const timer = setInterval(refresh, MACHINE_STATUS_REFRESH_MS);
    return () => clearInterval(timer);
  }, [activeEnv]);

  function toggleLevel(level: string) {
    setSelectedLevels((prev) => {
      const next = new Set(prev);
      if (next.has(level)) {
        next.delete(level);
      } else {
        next.add(level);
      }
      return next;
    });
  }

  function togglePluginName(name: string) {
    setSelectedPluginNames((prev) => {
      const next = new Set(prev);
      if (next.has(name)) {
        next.delete(name);
      } else {
        next.add(name);
      }
      return next;
    });
  }

  // 筛选已下推到后端，这里直接用返回的条目——不再做一次前端全量过滤
  // （原实现每次 render 都重算一遍，且没有 useMemo）
  const filteredLogEntries = logEntries;
  // 还有未加载的命中条目时显示「加载更多」
  const hasMoreLogs = logEntries.length < logTotal;

  async function handleOpenLogDirFromLogsTab() {
    try {
      await invoke<string>("open_log_dir");
    } catch (e) {
      setLogError(`打开日志目录失败: ${e}`);
    }
  }

  async function loadAppInfo(env: Env) {
    try {
      const info = await invoke<UpdateInfo>("get_app_info", { env });
      setAppInfo(info);
    } catch (e) {
      setStatus(`获取信息失败: ${e}`);
    }
  }

  /** 点击编辑路径按钮：进入编辑状态，回显当前路径 */
  function handleEditPath() {
    setCustomPathInput(appInfo?.install_path ?? "");
    setEditingPath(true);
  }

  /** 调用系统文件夹选择对话框 */
  async function handleBrowsePath() {
    try {
      const selected = await open({ directory: true, multiple: false });
      if (selected && typeof selected === "string") {
        setCustomPathInput(selected);
      }
    } catch (e) {
      setStatus(`打开目录选择失败: ${e}`);
    }
  }

  /** 保存自定义路径 */
  async function handleSavePath() {
    const trimmed = customPathInput.trim();
    if (!trimmed) {
      setStatus("路径不能为空");
      return;
    }
    try {
      await invoke("save_custom_path", { env: activeEnv, path: trimmed });
      setEditingPath(false);
      setStatus("安装路径已保存");
      await loadAppInfo(activeEnv);
    } catch (e) {
      setStatus(`保存路径失败: ${e}`);
    }
  }

  /** 取消编辑 */
  function handleCancelEdit() {
    setEditingPath(false);
    setCustomPathInput("");
  }

  async function handleCheckUpdate() {
    setLoading(true);
    setStatus("正在检查更新...");
    try {
      const result = await invoke<CheckResult>("check_update", { env: activeEnv });
      setCheckResult(result);
      if (result.has_update) {
        setStatus(`发现新版本: ${result.remote_version}（当前: ${result.current_version}）`);
        setShowConfirm(true);
      } else {
        setStatus(`当前已是最新版本，无需更新（本地: ${result.current_version}，线上: ${result.remote_version}）`);
      }
    } catch (e) {
      setStatus(`检查更新失败: ${e}`);
    } finally {
      setLoading(false);
    }
  }

  async function handleConfirmUpdate() {
    setShowConfirm(false);
    setLoading(true);
    setStatus("正在下载并安装更新，请稍候...");
    try {
      const result = await invoke<string>("perform_update", { env: activeEnv });
      setStatus(result);
      await loadAppInfo(activeEnv);
      // 更新完成后：自动刷新所有 Chrome 标签页
      await postUpdateChromeActions();
    } catch (e) {
      setStatus(`更新失败: ${e}`);
    } finally {
      setLoading(false);
    }
  }

  /** 更新完成后自动刷新所有 Chrome 标签页 */
  async function postUpdateChromeActions() {
    try {
      const refreshResult = await invoke<string>("refresh_chrome_tabs");
      setStatus((prev) => `${prev}；${refreshResult}`);
    } catch (e) {
      // 刷新失败不阻断主流程，仅提示
      setStatus((prev) => `${prev}；刷新Chrome失败: ${e}`);
    }
  }

  return (
    <main
      className={
        // 所有页面统一宽布局：窗口默认已加宽到 900px（巡检页 8 列表格需要），
        // 只让部分页面放开的话，切 tab 时内容区宽度会左右跳动
        "container container-wide"
      }
    >
      <div className="view-tabs">
        <button
          className={`view-tab-btn ${view === "patrol" ? "view-tab-active" : ""}`}
          onClick={() => setView("patrol")}
        >
          插件巡检
        </button>
        <button
          className={`view-tab-btn ${view === "update" ? "view-tab-active" : ""}`}
          onClick={() => setView("update")}
        >
          更新管理
        </button>
        <button
          className={`view-tab-btn ${view === "logs" ? "view-tab-active" : ""}`}
          onClick={() => setView("logs")}
        >
          日志查看
        </button>
        <button
          className={`view-tab-btn ${view === "machine" ? "view-tab-active" : ""}`}
          onClick={() => setView("machine")}
        >
          机器状态
        </button>
        <button
          className={`view-tab-btn ${view === "chrome-mapping" ? "view-tab-active" : ""}`}
          onClick={() => setView("chrome-mapping")}
          title="独立进程架构（各实例各自 --user-data-dir 启动）专用：确认目录名对应哪个插件实例，用于精确定位打开侧边栏"
        >
          实例映射
        </button>
        {/* 当前版本常驻显示：排查多台机器时不必再去翻日志 */}
        {appVersion && (
          <span className="app-version">
            {/*
              自动更新关闭时常驻提示（2026-08-27）。
              放在版本号旁边而不是某个页面里——关掉影响的是整个客户端，
              任何页面都该看得见。

              为什么非要有这个提示：默认关意味着新装的机器永远停在安装时
              那个版本，而「关了忘记开」是这类开关的典型问题。2026-08-24 刚
              经历过「自动更新链路坏了 6 天没人发现」，静默不升级同样难察觉。
              紧挨着「检查更新」按钮，看到提示就知道该点哪儿。
            */}
            {!autoUpdate && (
              <span
                className="auto-update-off"
                title="自动检查更新已关闭（托盘菜单可开启）。当前版本不会自动升级，需手动点「检查更新」"
              >
                自动更新已关
              </span>
            )}
            <button
              className="check-update-btn"
              onClick={() => runSelfUpdateCheck("manual")}
              disabled={checkingUpdate}
              title="立即检查是否有新版本"
            >
              {checkingUpdate ? "检查中…" : "检查更新"}
            </button>
            v{appVersion}
          </span>
        )}
        {/* 机器负载简化指示器：绝对定位常驻显示，无需切到「机器状态」页才能看到，
            负载过高时变红提醒。位置相对 tab 栏右下角上移/右移（见 CSS 的 right/bottom） */}
        {systemSnapshot && (
          <span
            className={`machine-status-indicator ${
              isHighLoad(systemSnapshot) ? "machine-status-indicator-high" : ""
            }`}
            title={`CPU 占用 ${systemSnapshot.cpu_usage_percent.toFixed(
              1
            )}%，内存可用 ${formatBytesAsGb(systemSnapshot.available_memory_bytes)}`}
          >
            CPU {systemSnapshot.cpu_usage_percent.toFixed(0)}% · 内存{" "}
            {(systemSnapshot.available_memory_bytes / 1024 / 1024 / 1024).toFixed(1)}G 可用
          </span>
        )}
      </div>

      {selfUpdateHint && <div className="self-update-hint">{selfUpdateHint}</div>}

      {selfUpdate.kind !== "idle" && (
        <div className="self-update-bar">
          {selfUpdate.kind === "downloading" ? (
            <span>正在后台下载新版本 {selfUpdate.version}…</span>
          ) : (
            <>
              <div className="self-update-text">
                <span>新版本 {selfUpdate.version} 已就绪，重启后生效</span>
                {selfUpdate.notes && (
                  <span className="self-update-notes">{selfUpdate.notes}</span>
                )}
              </div>
              <button className="self-update-btn" onClick={() => relaunch()}>
                立即重启
              </button>
            </>
          )}
        </div>
      )}

      {view === "logs" ? (
        <div className="log-view">
          <div className="log-toolbar">
            <select
              className="log-date-select"
              value={selectedDate}
              onChange={(e) => setSelectedDate(e.target.value)}
              disabled={logDates.length === 0}
            >
              {logDates.length === 0 ? (
                <option value="">暂无日志</option>
              ) : (
                logDates.map((d) => (
                  <option key={d} value={d}>
                    {d}
                  </option>
                ))
              )}
            </select>

            <div className="log-time-range">
              <input
                type="time"
                className="log-time-input"
                value={timeFrom}
                onChange={(e) => setTimeFrom(e.target.value)}
                title="开始时间"
              />
              <span className="log-time-sep">~</span>
              <input
                type="time"
                className="log-time-input"
                value={timeTo}
                onChange={(e) => setTimeTo(e.target.value)}
                title="结束时间"
              />
            </div>

            <input
              className="log-search-input"
              type="text"
              placeholder="搜索关键词..."
              value={keyword}
              onChange={(e) => setKeyword(e.target.value)}
            />

            <button className="btn-secondary" onClick={handleOpenLogDirFromLogsTab}>
              📁 打开日志目录
            </button>
          </div>

          <div className="log-toolbar log-toolbar-filters">
            <div className="log-filter-group">
              <span className="log-filter-label">级别:</span>
              {ALL_LEVELS.map((level) => (
                <label
                  key={level}
                  className={`log-filter-chip log-level-${level.toLowerCase()} ${
                    selectedLevels.has(level) ? "log-filter-chip-active" : ""
                  }`}
                >
                  <input
                    type="checkbox"
                    checked={selectedLevels.has(level)}
                    onChange={() => toggleLevel(level)}
                  />
                  {level}
                </label>
              ))}
            </div>

            {availablePluginNames.length > 0 && (
              <div className="log-filter-group">
                <span className="log-filter-label">插件名:</span>
                {availablePluginNames.map((name) => (
                  <label
                    key={name}
                    className={`log-filter-chip ${
                      selectedPluginNames.has(name) ? "log-filter-chip-active" : ""
                    }`}
                  >
                    <input
                      type="checkbox"
                      checked={selectedPluginNames.has(name)}
                      onChange={() => togglePluginName(name)}
                    />
                    {name}
                  </label>
                ))}
              </div>
            )}
          </div>

          {logError && <div className="status-msg error">{logError}</div>}

          <div className="log-table-wrap">
            {logLoading ? (
              <div className="log-empty">加载中...</div>
            ) : filteredLogEntries.length === 0 ? (
              <div className="log-empty">
                {logDates.length === 0 ? "暂无日志文件" : "没有符合条件的日志"}
              </div>
            ) : (
              <table className="log-table">
                <thead>
                  <tr>
                    <th className="col-ts">时间</th>
                    <th className="col-level">级别</th>
                    <th className="col-source">来源</th>
                    <th className="col-plugin">插件名</th>
                    <th className="col-message">消息</th>
                  </tr>
                </thead>
                <tbody>
                  {filteredLogEntries.map((entry, idx) => (
                    <tr key={idx}>
                      <td className="col-ts">{entry.timestamp}</td>
                      <td className="col-level">
                        <span className={`log-level-badge log-level-${entry.level.toLowerCase()}`}>
                          {entry.level}
                        </span>
                      </td>
                      <td className="col-source">{entry.source}</td>
                      <td className="col-plugin">{entry.plugin_name}</td>
                      <td className="col-message">{entry.message}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            )}
            {!logLoading && filteredLogEntries.length > 0 ? (
              <div className="log-page-footer">
                <span className="log-page-count">
                  已显示 {filteredLogEntries.length} / 共 {logTotal} 条
                </span>
                {hasMoreLogs ? (
                  <button
                    className="log-load-more"
                    onClick={() => setLogLoadedPages((p) => p + 1)}
                  >
                    加载更多
                  </button>
                ) : null}
              </div>
            ) : null}
          </div>
        </div>
      ) : view === "patrol" ? (
        /* 插件巡检看板（DEV-125034）：替代「逐个点开 15 个浏览器」的日常巡检。
           实例按插件名中的数字从小到大排序，方便对着虚拟机逐台核对 */
        <div className="patrol-view">
          <div className="patrol-summary">
            <span>
              插件 {patrol?.instances.length ?? 0} 个
              {patrol && patrol.instances.some((i) => i.hasIssue) && (
                <b className="patrol-issue-count">
                  ，{patrol.instances.filter((i) => i.hasIssue).length} 个异常
                </b>
              )}
            </span>
            <span>
              Chrome 窗口 {patrol?.chromeWindows ?? 0} 个
              {(patrol?.minimizedWindows ?? 0) > 0 &&
                `（${patrol?.minimizedWindows} 个已最小化）`}
            </span>
            {patrolHint && <span className="patrol-hint">{patrolHint}</span>}
          </div>
          <div className="log-table-wrap">
            {!patrol || patrol.instances.length === 0 ? (
              <div className="log-empty">
                暂无插件上报心跳。确认插件已安装并打开侧边栏，且客户端日志服务已启动
              </div>
            ) : (
              <table className="log-table">
                <thead>
                  <tr>
                    <th className="col-index">#</th>
                    <th className="col-name">插件名称</th>
                    <th className="col-heartbeat">心跳</th>
                    <th className="col-status">侧边栏</th>
                    <th className="col-status">WS</th>
                    <th className="col-status">1688</th>
                    <th className="col-status">状态</th>
                    <th className="col-cj">CJ账号</th>
                    <th>1688账号</th>
                    <th className="col-actions">操作</th>
                  </tr>
                </thead>
                <tbody>
                  {patrol.instances.map((it, idx) => (
                    <tr key={it.pluginName} className={it.hasIssue ? "patrol-row-issue" : ""}>
                      {/* 序号仅供口头指代（「第 3 行那台」），不是稳定标识——
                          后端按插件名中的数字排序，实例增减时序号仍会变化 */}
                      <td className="patrol-index">{idx + 1}</td>
                      {/* 实例名可能被截断（表格不做横向滚动），title 里给全名 */}
                      <td className="patrol-name" title={it.pluginName}>
                        {it.pluginName}
                      </td>
                      <td className={it.stale ? "patrol-bad" : "patrol-ok"}>
                        {it.stale ? `失联 ${it.silenceSecs}s` : `${it.silenceSecs}s 前`}
                      </td>
                      <td className={it.sidepanelOpen ? "patrol-ok" : "patrol-bad"}>
                        {it.sidepanelOpen ? "已打开" : "未打开"}
                      </td>
                      <td
                        className={
                          it.wsConnected === null
                            ? ""
                            : it.wsConnected
                              ? "patrol-ok"
                              : "patrol-bad"
                        }
                      >
                        {it.wsConnected === null ? "—" : it.wsConnected ? "已连接" : "已断开"}
                      </td>
                      <td
                        className={
                          it.login1688 === null ? "" : it.login1688 ? "patrol-ok" : "patrol-bad"
                        }
                      >
                        {it.login1688 === null ? "—" : it.login1688 ? "已登录" : "未登录"}
                      </td>
                      <td>{it.taskRunning ? "运行中" : "空闲"}</td>
                      {/* CJ 账号：这台机器归谁用。它是查「应绑 1688 账号」的入口 */}
                      <td className="patrol-account" title={it.cjAccount ?? ""}>
                        {it.cjAccount ?? "—"}
                      </td>
                      {/* 1688 账号：串号时把两个都显示出来，否则光说「不一致」没法判断该改哪边。
                          完整内容放 title——这列会被截断（表格不做横向滚动） */}
                      <td
                        className={`patrol-account ${it.accountMismatch ? "patrol-bad" : ""}`}
                        title={
                          it.accountMismatch
                            ? `应为 ${it.expectedAccount}，实为 ${it.actualAccount}`
                            : (it.actualAccount ?? "")
                        }
                      >
                        {it.accountMismatch
                          ? `串号：应为 ${it.expectedAccount}，实为 ${it.actualAccount}`
                          : (it.actualAccount ?? "—")}
                      </td>
                      {/*
                        操作列（DEV-125035）。四个按钮按「会不会让插件失去干活能力」
                        排序与配色，最危险的放最后并单独标红：
                        - 重连WS / 刷新 / 登录：都不关侧边栏，插件仍能干活
                        - 重载：会销毁侧边栏，而侧边栏无法由代码重开
                        全部只在人点击时触发，不接任何自动判定——两次教训
                        （抢焦点、登录死循环）都出在自动触发上
                      */}
                      <td className="patrol-actions">
                        <button
                          className="patrol-btn"
                          disabled={sendingCmd !== null}
                          onClick={() => sendCommand(it.pluginName, "reconnectWs")}
                          title="重新建立插件与业务服务端的 WebSocket 连接。不影响侧边栏"
                        >
                          重连WS
                        </button>
                        <button
                          className="patrol-btn"
                          disabled={sendingCmd !== null}
                          onClick={() => sendCommand(it.pluginName, "refreshSidepanel")}
                          title="刷新：重载侧边栏页面，重置卡住的状态机。侧边栏不会关闭，有任务在跑时插件会拒绝"
                        >
                          刷新
                        </button>
                        <button
                          className="patrol-btn"
                          disabled={sendingCmd !== null}
                          onClick={() => sendCommand(it.pluginName, "trigger1688Login")}
                          title="登录 1688：在后台标签页打开登录页，由插件自动完成登录。不抢焦点"
                        >
                          登录1688
                        </button>
                        <button
                          className="patrol-btn patrol-btn-danger"
                          disabled={sendingCmd !== null}
                          onClick={() => sendCommand(it.pluginName, "reload")}
                          title="重载插件：会关闭侧边栏且无法由代码重开，插件将无法干活直到有人手动打开——仅在侧边栏已经挂掉时使用"
                        >
                          重载
                        </button>
                        {/*
                          打开侧边栏（DEV-125986）。只在侧边栏确实关闭时才显示——
                          已打开时点它没有意义，还会误导人以为需要点一下。
                          与顶部兜底按钮的区别：这里带上 pluginName，后端会精确
                          定位到这一个实例的窗口再点击，不影响其它正常实例。
                        */}
                        {!it.sidepanelOpen && (
                          <button
                            className="patrol-btn patrol-btn-danger"
                            disabled={sendingCmd !== null}
                            onClick={() => clickPluginIcon(it.pluginName)}
                            title={`用图像识别精确定位 ${it.pluginName} 的插件图标并点击，用于重新打开侧边栏。⚠️ 会把这个实例的 Chrome 窗口抢到最前并移动鼠标`}
                          >
                            打开侧边栏
                          </button>
                        )}
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            )}
          </div>
        </div>
      ) : view === "machine" ? (
        <div className="machine-status-view">
          {systemSnapshot ? (
            <div className="machine-status-grid">
              <div className="machine-status-card">
                <span className="machine-status-label">内存</span>
                <span className="machine-status-value">
                  {formatBytesAsGb(systemSnapshot.available_memory_bytes)} 可用 / 共{" "}
                  {formatBytesAsGb(systemSnapshot.total_memory_bytes)}
                </span>
              </div>
              <div className="machine-status-card">
                <span className="machine-status-label">CPU</span>
                <span className="machine-status-value">
                  {systemSnapshot.cpu_brand}（{systemSnapshot.cpu_cores} 核）
                </span>
                <span className="machine-status-value machine-status-emphasis">
                  当前占用 {systemSnapshot.cpu_usage_percent.toFixed(1)}%
                </span>
              </div>
              <div className="machine-status-card">
                <span className="machine-status-label">磁盘可用空间（安装目录所在盘）</span>
                <span className="machine-status-value">
                  {formatBytesAsGb(systemSnapshot.disk_available_bytes)} 可用 / 共{" "}
                  {formatBytesAsGb(systemSnapshot.disk_total_bytes)}
                </span>
              </div>
              <div className="machine-status-card">
                <span className="machine-status-label">系统版本</span>
                <span className="machine-status-value">{systemSnapshot.os_version}</span>
              </div>
            </div>
          ) : (
            <p className="machine-status-loading">正在获取机器状态…</p>
          )}
          {/* 「打开插件侧边栏」按钮已于 2026-08-24 移除。
              它是客户端里最后一个会抢全局焦点的入口：实现只能靠
              AppActivate + SendKeys 模拟按键，而焦点是全局唯一资源——
              一台机器跑 3~15 个 Chrome 实例、插件正通过它们往供应商聊天框
              输入文字，抢一次焦点就会打断其中正在输入的那个（不限于目标实例），
              按键可能落进聊天框造成乱字符或丢字，污染发给供应商的内容。

              保留它原本的理由是「人工排查时确认快捷键链路是否通」，但该链路
              已确认不用（Chrome 强制 sidePanel.open() 由用户手势触发，
              自动拉起整条路走不通）。采购机无人值守，也没人需要点它。
              真要人工开侧边栏，远程桌面进去按 Ctrl+Shift+L 即可。 */}
        </div>
      ) : view === "chrome-mapping" ? (
        <div className="chrome-mapping-view">
          {/*
            独立进程架构实例映射确认（DEV-125986）。

            真机实测确认：这类架构下（各实例各自 --user-data-dir 启动），
            目录名与 plugin_name 之间没有任何程序能读到的客观对应关系——
            需要人工在这里做一次性选择确认，之后除非实例有变动才需要
            重新配置。单进程多 Profile 架构（Profile 已命名）不需要这个
            页面，走全自动的 UI Automation 识别。
          */}
          <p className="chrome-mapping-intro">
            仅「每个插件实例各自用独立 Chrome 快捷方式（各自 --user-data-dir）启动」的机器需要配置。
            为每个检测到的 Chrome 数据目录选择对应的插件实例，保存后「打开侧边栏」按钮才能精确点击到这个实例。
          </p>
          {/*
            "刷新候选"按钮：改为人工触发而非进入页面/切 tab 自动拉取。
            早期版本用 useEffect([view]) 自动加载，副作用是切 tab 来回
            会用 savedMapping 覆盖掉正在编辑但未保存的选择——选了半天，
            切一下 tab 就白选。改成按钮后不会有任何意外时机的重新拉取。
          */}
          <button
            className="chrome-mapping-refresh-btn"
            onClick={loadChromeProfileCandidates}
            disabled={mappingSaving}
          >
            {chromeCandidates ? "刷新候选" : "检测 Chrome 进程与在线实例"}
          </button>
          {mappingHint && <p className="chrome-mapping-hint">{mappingHint}</p>}
          {!chromeCandidates ? (
            <p className="chrome-mapping-loading">点上面的按钮开始检测</p>
          ) : chromeCandidates.directoryNames.length === 0 ? (
            <p className="chrome-mapping-empty">
              未检测到独立启动的 Chrome 进程（本机可能是单进程多 Profile 架构，不需要在这里配置）
            </p>
          ) : (
            <>
              <table className="chrome-mapping-table">
                <thead>
                  <tr>
                    <th>Chrome 数据目录</th>
                    <th>对应的插件实例</th>
                  </tr>
                </thead>
                <tbody>
                  {chromeCandidates.directoryNames.map((dir) => (
                    <tr key={dir}>
                      <td className="chrome-mapping-dir" title={dir}>
                        {dir}
                      </td>
                      <td>
                        <select
                          value={mappingDraft[dir] ?? ""}
                          onChange={(e) =>
                            setMappingDraft((prev) => ({ ...prev, [dir]: e.target.value }))
                          }
                          disabled={mappingSaving}
                        >
                          <option value="">-- 未选择 --</option>
                          {chromeCandidates.onlinePluginNames.map((name) => (
                            <option key={name} value={name}>
                              {name}
                            </option>
                          ))}
                        </select>
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
              <button
                className="chrome-mapping-save-btn"
                onClick={saveChromeProfileMapping}
                disabled={mappingSaving}
              >
                保存映射
              </button>
            </>
          )}
        </div>
      ) : (
        <>
      <div className="env-tabs">
        <button
          className={`tab-btn ${activeEnv === "online" ? "tab-active tab-online" : ""}`}
          onClick={() => !loading && setActiveEnv("online")}
          disabled={loading}
        >
          线上环境
        </button>
        <button
          className={`tab-btn ${activeEnv === "test" ? "tab-active tab-test" : ""}`}
          onClick={() => !loading && setActiveEnv("test")}
          disabled={loading}
        >
          测试环境
        </button>
      </div>

      <div className="info-card">
        <div className="info-row">
          <span className="label">当前版本:</span>
          <span className="value">{appInfo?.current_version || "加载中..."}  </span>
        </div>

        {/* 安装路径行：支持编辑 */}
        <div className="info-row">
          <span className="label">安装路径:</span>
          {editingPath ? (
            <div className="path-edit-group">
              <input
                className="path-input"
                value={customPathInput}
                onChange={(e) => setCustomPathInput(e.target.value)}
                placeholder="请输入安装路径，如 C:\aichat"
                disabled={loading}
              />
              <button className="btn-icon" onClick={handleBrowsePath} disabled={loading} title="浏览文件夹">
                📁
              </button>
              <button className="btn-icon btn-confirm" onClick={handleSavePath} disabled={loading} title="保存">
                ✅
              </button>
              <button className="btn-icon btn-cancel" onClick={handleCancelEdit} disabled={loading} title="取消">
                ❌
              </button>
            </div>
          ) : (
            <div className="path-display-group">
              <span className="value path">{appInfo?.install_path || "加载中..."}</span>
              <button
                className="btn-edit"
                onClick={handleEditPath}
                disabled={loading || !appInfo}
                title="修改安装路径"
              >
                ✏️ 修改
              </button>
            </div>
          )}
        </div>

        <div className="info-row">
          <span className="label">下载地址:</span>
          <span className="value path">{appInfo?.download_url || "加载中..."}</span>
        </div>
      </div>

      <div className="actions">
        <button
          className="btn-primary"
          onClick={handleCheckUpdate}
          disabled={loading}
        >
          {loading ? "处理中..." : "🔄 立即检查更新"}
        </button>
      </div>

      {status && (
        <div className={`status-msg ${status.includes("失败") ? "error" : status.includes("完成") || status.includes("最新") || status.includes("已保存") || status.includes("已打开") ? "success" : "info"}`}>
          {status}
        </div>
      )}

      {showConfirm && (
        <div className="confirm-overlay">
          <div className="confirm-dialog">
            <h3>确认更新</h3>
            <p>发现新版本 <strong>{checkResult?.remote_version}</strong></p>
            <p>当前版本: {checkResult?.current_version}</p>
            <p>安装路径: {checkResult?.install_path}</p>
            <div className="confirm-actions">
              <button className="btn-primary" onClick={handleConfirmUpdate}>
                确定更新
              </button>
              <button className="btn-secondary" onClick={() => { setShowConfirm(false); setStatus(""); }}>
                取消
              </button>
            </div>
          </div>
        </div>
      )}
        </>
      )}
    </main>
  );
}

export default App;
