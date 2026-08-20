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

interface LogEntriesResult {
  entries: LogEntry[];
  plugin_names: string[];
}

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

type View = "update" | "logs" | "machine";

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

/// 上一次记录过的自更新错误，用于抑制重复日志（见 runSelfUpdateCheck）
let lastSelfUpdateError: string | null = null;

/// 更新器自身的更新状态
type SelfUpdateState =
  | { kind: "idle" }
  | { kind: "downloading"; version: string }
  | { kind: "ready"; version: string; notes: string };

function App() {
  const [view, setView] = useState<View>("update");
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

  // 机器状态：CPU/内存/磁盘/系统版本，辅助判断虚拟机是否卡顿
  const [systemSnapshot, setSystemSnapshot] = useState<SystemSnapshot | null>(null);
  // 手动打开插件侧边栏的结果提示（自愈流程会自动调，这里供排查时手动触发）
  const [sidepanelHint, setSidepanelHint] = useState<string>("");

  // 更新器自身的自动更新（区别于上面「更新 aichat 插件」的业务逻辑）
  const [selfUpdate, setSelfUpdate] = useState<SelfUpdateState>({ kind: "idle" });
  // 当前程序版本，显示在界面上——此前只能翻日志确认，排查十几台机器时很不方便
  const [appVersion, setAppVersion] = useState<string>("");
  // 手动检查更新的状态与反馈（自动轮询不产生这些提示）
  const [checkingUpdate, setCheckingUpdate] = useState<boolean>(false);
  const [selfUpdateHint, setSelfUpdateHint] = useState<string>("");

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

    runSelfUpdateCheck("auto");
    const timer = setInterval(() => runSelfUpdateCheck("auto"), SELF_UPDATE_INTERVAL_MS);
    return () => clearInterval(timer);
  }, []);

  /// 执行一次自更新检查。
  /// trigger 为 "manual" 时会把结果反馈到界面（采购同事点了按钮需要看到回应），
  /// "auto" 时保持安静——后台轮询不应打扰正在干活的人。
  async function runSelfUpdateCheck(trigger: "auto" | "manual") {
    if (checkingUpdate) return; // 防止连点或与轮询重入
    setCheckingUpdate(true);
    if (trigger === "manual") setSelfUpdateHint("正在检查更新…");
    try {
      const update = await check();
      lastSelfUpdateError = null; // 本次成功，解除同错误抑制
      if (!update) {
        if (trigger === "manual") setSelfUpdateHint("已是最新版本");
        return;
      }
      setSelfUpdateHint("");
      // 先确认清单里有当前系统的包，再进入下载状态。
      // 否则在只发布 Windows 包的情况下，macOS 上会先切成「正在下载」
      // 再抛错——用户看到的是永远转不完的下载提示。
      //
      // 清单的平台键形如 windows-x86_64 / darwin-aarch64，前缀即系统名。
      // 只按前缀做保守判断：一个都不沾边时才跳过，避免误判导致该升的没升
      const platforms = Object.keys(
        (update.rawJson?.platforms ?? {}) as Record<string, unknown>
      );
      const osPrefix = navigator.userAgent.includes("Windows")
        ? "windows"
        : navigator.userAgent.includes("Mac")
          ? "darwin"
          : "";
      const hasPlatform =
        platforms.length === 0 || // 清单没声明平台，交给插件自行处理
        !osPrefix ||              // 认不出系统，不擅自跳过
        platforms.some((k) => k.startsWith(osPrefix));
      if (!hasPlatform) {
        if (trigger === "manual") setSelfUpdateHint("当前系统暂无更新包");
        return;
      }
      setSelfUpdate({ kind: "downloading", version: update.version });
      // 拆成 download + install 两步而非 downloadAndInstall()：后者失败时
      // 无法判断卡在下载还是安装，而这两者的成因完全不同（网络 vs 权限/占用）。
      // 进度也一并落盘——上次故障机「一直显示下载中」，无从判断是没开始、
      // 下到一半断了、还是下完卡在安装
      let received = 0;
      let total = 0;
      await update.download((ev) => {
        if (ev.event === "Started") total = ev.data.contentLength ?? 0;
        else if (ev.event === "Progress") received += ev.data.chunkLength;
      });
      invoke("log_self_update_info", {
        message: `下载完成 ${received}/${total} 字节，开始安装 ${update.version}`,
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
      // 下载/安装失败必须把界面状态收回来。否则提示条会永远停在
      // 「正在后台下载…」——既不报错也不消失，比直接说失败更让人困惑。
      // 断网、站点 502、磁盘满都会走到这里，不只是平台不匹配
      setSelfUpdate({ kind: "idle" });
      // 手动触发时把真实错误显示出来。此前只提示「请稍后再试」、原因仅进日志，
      // 排查者必须去翻日志文件才知道发生了什么——而自更新失败恰恰是最需要
      // 现场信息的场景。自动轮询仍保持安静，不打扰正在干活的采购同事
      if (trigger === "manual") setSelfUpdateHint(`更新失败：${msg}`);
      // 当前平台不在清单里属正常情况（只发布 Windows 包），不是故障：
      // 记成 ERROR 会污染「仅异常」视图，且每轮必然复现、长期累积
      const isPlatformMissing = msg.includes("were found in the response");
      // 同一错误连续出现时只记一次——某台机器长期断网时，
      // 每 4 小时记一条相同内容，一个月会攒出上百条无用日志
      const isRepeat = msg === lastSelfUpdateError;
      if (!isPlatformMissing && !isRepeat) {
        lastSelfUpdateError = msg;
        // 自更新失败不弹错打扰用户：采购同事看到红色报错既看不懂也无从处理。
        // 写进日志文件，由排查者从日志页查看。
        invoke("log_self_update_error", { message: msg }).catch(() => {});
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

  // 日期变化时重新读取当天全量日志；级别/插件名/时间段/关键词都是对这份
  // 内存数据做组合过滤，不用每次筛选变化都重新调用后端命令
  useEffect(() => {
    if (view !== "logs" || !selectedDate) return;
    setLogLoading(true);
    setLogError("");
    invoke<LogEntriesResult>("read_log_entries", { date: selectedDate, errorOnly: false })
      .then((result) => {
        setLogEntries(result.entries);
        setAvailablePluginNames(result.plugin_names);
        // 默认全选当天出现过的插件名，且旧筛选状态不跨日期保留
        setSelectedPluginNames(new Set(result.plugin_names));
      })
      .catch((e) => setLogError(`读取日志失败: ${e}`))
      .finally(() => setLogLoading(false));
  }, [view, selectedDate]);

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

  const filteredLogEntries = logEntries.filter((e) => {
    if (!selectedLevels.has(e.level.toUpperCase())) return false;
    if (!selectedPluginNames.has(e.plugin_name)) return false;
    if (timeFrom || timeTo) {
      // timestamp 可能是 "HH:mm:ss.SSS" 或 ISO 格式，取时间部分的 HH:mm 比较
      const match = e.timestamp.match(/(\d{2}:\d{2})/);
      const hm = match ? match[1] : "";
      if (timeFrom && hm < timeFrom) return false;
      if (timeTo && hm > timeTo) return false;
    }
    if (keyword.trim()) {
      const kw = keyword.trim().toLowerCase();
      if (!(e.message + e.source + e.level + e.plugin_name).toLowerCase().includes(kw)) {
        return false;
      }
    }
    return true;
  });

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
  /// 手动触发打开插件侧边栏。
  /// 自愈流程（DEV-124702）会在重启 Chrome 后自动调用同一个命令；
  /// 这里保留手动入口，供排查时确认快捷键链路是否通
  async function handleOpenSidepanel() {
    setSidepanelHint("正在打开侧边栏…");
    try {
      const result = await invoke<string>("open_plugin_sidepanel");
      setSidepanelHint(result);
    } catch (e) {
      setSidepanelHint(`打开侧边栏失败: ${e}`);
    }
  }

  /// 手动触发「重启 Chrome + 打开侧边栏」（二级自愈的完整动作）。
  /// 代价较大（用户标签页会重开），故加二次确认；自愈流程会在插件
  /// 完全无响应时自动调用同一命令
  async function handleRestartChrome() {
    if (!confirm("将关闭并重新打开 Chrome，当前标签页会重新加载。确定继续？")) {
      return;
    }
    setSidepanelHint("正在重启 Chrome，请稍候…");
    try {
      const result = await invoke<string>("restart_chrome_and_open_sidepanel");
      setSidepanelHint(result);
    } catch (e) {
      setSidepanelHint(`重启 Chrome 失败: ${e}`);
    }
  }

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
    <main className={view === "logs" ? "container container-wide" : "container"}>
      <div className="view-tabs">
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
        {/* 当前版本常驻显示：排查多台机器时不必再去翻日志 */}
        {appVersion && (
          <span className="app-version">
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
          {/* 插件侧边栏必须常驻打开才能处理任务。自愈流程会自动拉起，
              这里保留手动入口供排查确认链路是否通 */}
          <div className="machine-status-actions">
            <button className="btn-secondary" onClick={handleOpenSidepanel}>
              打开插件侧边栏
            </button>
            <button className="btn-secondary" onClick={handleRestartChrome}>
              重启 Chrome
            </button>
            {sidepanelHint && (
              <span className="machine-status-hint">{sidepanelHint}</span>
            )}
          </div>
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
