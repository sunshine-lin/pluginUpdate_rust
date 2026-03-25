import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import "./App.css";

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
  const [appInfo, setAppInfo] = useState<UpdateInfo | null>(null);
  const [status, setStatus] = useState<string>("");
  const [loading, setLoading] = useState<boolean>(false);
  const [showConfirm, setShowConfirm] = useState<boolean>(false);
  const [checkResult, setCheckResult] = useState<CheckResult | null>(null);

  useEffect(() => {
    loadAppInfo();
  }, []);

  async function loadAppInfo() {
    try {
      const info = await invoke<UpdateInfo>("get_app_info");
      setAppInfo(info);
    } catch (e) {
      setStatus(`获取信息失败: ${e}`);
    }
  }

  async function handleCheckUpdate() {
    setLoading(true);
    setStatus("正在检查更新...");
    try {
      const result = await invoke<CheckResult>("check_update");
      setCheckResult(result);
      if (result.has_update) {
        setStatus(`发现新版本: ${result.remote_version}（当前: ${result.current_version}）`);
        setShowConfirm(true);
      } else {
        setStatus(`当前已是最新版本 (${result.current_version})，无需更新`);
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
      const result = await invoke<string>("perform_update");
      setStatus(result);
      await loadAppInfo();
    } catch (e) {
      setStatus(`更新失败: ${e}`);
    } finally {
      setLoading(false);
    }
  }

  const envLabel = appInfo?.env === "test" ? "测试环境" : "线上环境";
  const envClass = appInfo?.env === "test" ? "env-test" : "env-online";

  return (
    <main className="container">
      <div className="header">
        <h1>aichat 插件更新工具</h1>
        <span className={`env-badge ${envClass}`}>{envLabel}</span>
      </div>

      <div className="info-card">
        <div className="info-row">
          <span className="label">当前环境:</span>
          <span className="value">{envLabel}</span>
        </div>
        <div className="info-row">
          <span className="label">当前版本:</span>
          <span className="value">{appInfo?.current_version || "加载中..."}</span>
        </div>
        <div className="info-row">
          <span className="label">安装路径:</span>
          <span className="value path">{appInfo?.install_path || "加载中..."}</span>
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
        <div className={`status-msg ${status.includes("失败") ? "error" : status.includes("完成") || status.includes("最新") ? "success" : "info"}`}>
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
