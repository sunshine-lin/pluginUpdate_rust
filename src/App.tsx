import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
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

type View = "update" | "logs";

const ALL_LEVELS = ["ERROR", "WARN", "INFO", "DEBUG"];

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
      </div>

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
