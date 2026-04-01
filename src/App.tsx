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

function App() {
  const [activeEnv, setActiveEnv] = useState<Env>("online");
  const [appInfo, setAppInfo] = useState<UpdateInfo | null>(null);
  const [status, setStatus] = useState<string>("");
  const [loading, setLoading] = useState<boolean>(false);
  const [showConfirm, setShowConfirm] = useState<boolean>(false);
  const [checkResult, setCheckResult] = useState<CheckResult | null>(null);
  // 路径编辑状态
  const [editingPath, setEditingPath] = useState<boolean>(false);
  const [customPathInput, setCustomPathInput] = useState<string>("");
  // Chrome 扩展 ID 配置状态
  const [extensionId, setExtensionId] = useState<string>("");
  const [editingExtId, setEditingExtId] = useState<boolean>(false);
  const [extIdInput, setExtIdInput] = useState<string>("");

  useEffect(() => {
    setStatus("");
    setShowConfirm(false);
    setCheckResult(null);
    setAppInfo(null);
    setEditingPath(false);
    loadAppInfo(activeEnv);
  }, [activeEnv]);

  // 加载扩展 ID（只在首次挂载时执行一次）
  useEffect(() => {
    invoke<string | null>("get_extension_id").then((id) => {
      if (id) setExtensionId(id);
    });
  }, []);

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

  /** 进入扩展 ID 编辑状态 */
  function handleEditExtId() {
    setExtIdInput(extensionId);
    setEditingExtId(true);
  }

  /** 保存 Chrome 扩展 ID */
  async function handleSaveExtId() {
    const trimmed = extIdInput.trim();
    if (!trimmed) {
      setStatus("扩展ID不能为空");
      return;
    }
    if (!/^[a-p]{32}$/.test(trimmed)) {
      setStatus("扩展ID格式无效，应为32位 a-p 小写字母");
      return;
    }
    try {
      await invoke("save_extension_id", { id: trimmed });
      setExtensionId(trimmed);
      setEditingExtId(false);
      setStatus("扩展 ID 已保存");
    } catch (e) {
      setStatus(`保存扩展ID失败: ${e}`);
    }
  }

  /** 通过 RPA 打开 Chrome 扩展侧边栏 */
  async function handleOpenSidebar() {
    if (!extensionId) {
      setStatus("请先配置 Chrome 扩展 ID");
      return;
    }
    setLoading(true);
    setStatus("正在打开 Chrome 扩展侧边栏...");
    try {
      const result = await invoke<string>("open_chrome_sidebar", { extensionId });
      setStatus(result);
    } catch (e) {
      setStatus(`打开侧边栏失败: ${e}`);
    } finally {
      setLoading(false);
    }
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
      // 更新完成后：自动刷新所有 Chrome 标签页，再打开侧边栏（如已配置扩展ID）
      await postUpdateChromeActions();
    } catch (e) {
      setStatus(`更新失败: ${e}`);
    } finally {
      setLoading(false);
    }
  }

  /**
   * 更新完成后的 Chrome 自动化操作：
   * 1. 刷新所有 Chrome 标签页
   * 2. 若已配置扩展 ID，自动打开侧边栏
   */
  async function postUpdateChromeActions() {
    try {
      const refreshResult = await invoke<string>("refresh_chrome_tabs");
      setStatus((prev) => `${prev}；${refreshResult}`);
    } catch (e) {
      // 刷新失败不阻断主流程，仅提示
      setStatus((prev) => `${prev}；刷新Chrome失败: ${e}`);
    }
    if (extensionId) {
      try {
        const sidebarResult = await invoke<string>("open_chrome_sidebar", { extensionId });
        setStatus((prev) => `${prev}；${sidebarResult}`);
      } catch (e) {
        setStatus((prev) => `${prev}；打开侧边栏失败: ${e}`);
      }
    }
  }

  return (
    <main className="container">
      <div className="header">
        <h1>aichat 插件更新工具</h1>
      </div>

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

        {/* Chrome 扩展 ID 行：用于侧边栏 RPA */}
        <div className="info-row">
          <span className="label">扩展 ID:</span>
          {editingExtId ? (
            <div className="path-edit-group">
              <input
                className="path-input"
                value={extIdInput}
                onChange={(e) => setExtIdInput(e.target.value)}
                placeholder="输入32位 Chrome 扩展 ID"
                maxLength={32}
                disabled={loading}
              />
              <button className="btn-icon btn-confirm" onClick={handleSaveExtId} disabled={loading} title="保存">
                ✅
              </button>
              <button className="btn-icon btn-cancel" onClick={() => setEditingExtId(false)} disabled={loading} title="取消">
                ❌
              </button>
            </div>
          ) : (
            <div className="path-display-group">
              <span className="value path">{extensionId || "未配置"}</span>
              <button
                className="btn-edit"
                onClick={handleEditExtId}
                disabled={loading}
                title="配置 Chrome 扩展 ID"
              >
                ✏️ 配置
              </button>
            </div>
          )}
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
        <button
          className="btn-sidebar"
          onClick={handleOpenSidebar}
          disabled={loading || !extensionId}
          title={!extensionId ? "请先配置扩展ID" : "打开 aichat 侧边栏"}
        >
          🔌 打开 aichat 侧边栏
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
    </main>
  );
}

export default App;
