import { startTransition, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import type {
  EnvironmentInfo,
  RenderStatus,
  StartRenderRequest,
  WorldInspection,
} from "./types";

const EMPTY_STATUS: RenderStatus = {
  state: "idle",
  job_id: null,
  world_path: null,
  job_dir: null,
  output_dir: null,
  preview_url: null,
  port: null,
  started_at: null,
  completed_at: null,
  elapsed_seconds: 0,
  progress: null,
  output_size_bytes: 0,
  output_file_count: 0,
  process_running: false,
  logs: [],
  error: null,
};

function formatBytes(bytes: number): string {
  if (!bytes) return "0 B";
  const units = ["B", "KB", "MB", "GB", "TB"];
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${value.toFixed(value >= 10 || unit === 0 ? 0 : 1)} ${units[unit]}`;
}

function formatDuration(seconds: number): string {
  if (!seconds) return "0 秒";
  const h = Math.floor(seconds / 3600);
  const m = Math.floor((seconds % 3600) / 60);
  const s = Math.floor(seconds % 60);
  if (h > 0) return `${h} 小时 ${m} 分`;
  if (m > 0) return `${m} 分 ${s} 秒`;
  return `${s} 秒`;
}

function statusLabel(status: RenderStatus): string {
  switch (status.state) {
    case "running":
      return status.progress ? "正在渲染" : "正在启动";
    case "completed":
      return "渲染完成，预览服务运行中";
    case "failed":
      return "渲染失败";
    case "stopped":
      return "已停止";
    default:
      return "等待开始";
  }
}

export default function App() {
  const [env, setEnv] = useState<EnvironmentInfo | null>(null);
  const [worldPath, setWorldPath] = useState("");
  const [outputRoot, setOutputRoot] = useState("");
  const [inspection, setInspection] = useState<WorldInspection | null>(null);
  const [status, setStatus] = useState<RenderStatus>(EMPTY_STATUS);
  const [advanced, setAdvanced] = useState(false);
  const [threads, setThreads] = useState(4);
  const [port, setPort] = useState(8100);
  const [renderNether, setRenderNether] = useState(true);
  const [renderEnd, setRenderEnd] = useState(true);
  const [activeView, setActiveView] = useState<"preview" | "logs">("preview");
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    void refreshEnvironment();
    const timer = window.setInterval(() => {
      void refreshStatus();
    }, 1200);
    return () => window.clearInterval(timer);
  }, []);

  async function refreshEnvironment() {
    const info = await invoke<EnvironmentInfo>("check_environment");
    setEnv(info);
    setThreads(info.system.recommended_threads);
  }

  async function refreshStatus() {
    const next = await invoke<RenderStatus>("get_render_status");
    startTransition(() => setStatus(next));
  }

  async function chooseWorld() {
    const selected = await open({
      directory: true,
      multiple: false,
      title: "选择 Minecraft Java 世界目录",
    });
    if (typeof selected !== "string") return;
    setWorldPath(selected);
    const result = await invoke<WorldInspection>("inspect_world", { worldPath: selected });
    setInspection(result);
    setRenderNether(result.has_nether);
    setRenderEnd(result.has_end);
  }

  async function chooseOutputRoot() {
    const selected = await open({
      directory: true,
      multiple: false,
      title: "选择输出目录",
    });
    if (typeof selected === "string") {
      setOutputRoot(selected);
    }
  }

  async function startRender() {
    if (!worldPath) return;
    setBusy(true);
    try {
      const request: StartRenderRequest = {
        world_path: worldPath,
        output_root: outputRoot || null,
        threads,
        port,
        render_nether: renderNether,
        render_end: renderEnd,
      };
      const next = await invoke<RenderStatus>("start_render", { request });
      setStatus(next);
    } finally {
      setBusy(false);
    }
  }

  async function stopRender() {
    setBusy(true);
    try {
      const next = await invoke<RenderStatus>("stop_render");
      setStatus(next);
    } finally {
      setBusy(false);
    }
  }

  async function openPreview() {
    await invoke("open_preview");
  }

  async function openOutput() {
    await invoke("open_output_folder");
  }

  async function exportDiagnostic() {
    const path = await invoke<string>("export_diagnostic_report");
    window.alert(`诊断报告已导出：\n${path}`);
  }

  const canStart =
    Boolean(worldPath) &&
    Boolean(inspection?.valid) &&
    Boolean(env?.java.found && (env.java.major ?? 0) >= 21) &&
    Boolean(env?.bluemap_jar_found) &&
    status.state !== "running";

  const progress = status.progress?.percent ?? 0;

  return (
    <main className="app-shell">
      <section className="workspace">
        <aside className="control-panel">
          <div className="section-title">
            <span>1</span>
            <h2>环境</h2>
          </div>
          <div className="check-grid">
            <CheckItem
              label="Java 21+"
              ok={Boolean(env?.java.found && (env.java.major ?? 0) >= 21)}
              value={env?.java.version ?? "未检测到"}
            />
            <CheckItem
              label="BlueMap CLI"
              ok={Boolean(env?.bluemap_jar_found)}
              value={env?.bluemap_jar_found ? "已找到" : "缺少 bin/BlueMap-cli.jar"}
            />
            <CheckItem
              label="系统"
              ok={Boolean(env)}
              value={env ? `${env.system.os} ${env.system.arch}` : "检测中"}
            />
            <CheckItem
              label="建议线程"
              ok={Boolean(env)}
              value={env ? `${env.system.recommended_threads} / ${env.system.cpu_count}` : "检测中"}
            />
          </div>

          <div className="section-title">
            <span>2</span>
            <h2>世界目录</h2>
          </div>
          <button className="primary-button" type="button" onClick={chooseWorld}>
            选择 Minecraft 世界
          </button>
          <PathBox value={worldPath || "尚未选择"} />

          {inspection && (
            <div className="inspection">
              <CheckItem label="level.dat" ok={inspection.has_level_dat} value={inspection.has_level_dat ? "存在" : "缺失"} />
              <CheckItem label="下界 DIM-1" ok={inspection.has_nether} value={inspection.has_nether ? "发现" : "未发现"} />
              <CheckItem label="末地 DIM1" ok={inspection.has_end} value={inspection.has_end ? "发现" : "未发现"} />
              <CheckItem label="世界大小" ok value={`${formatBytes(inspection.size_bytes)} / ${inspection.file_count} 个文件`} />
              <CheckItem label="预计磁盘" ok value={formatBytes(inspection.estimated_required_bytes)} />
              {inspection.warnings.map((warning) => (
                <p className="warning" key={warning}>{warning}</p>
              ))}
            </div>
          )}

          <button
            className="ghost-button"
            type="button"
            onClick={() => setAdvanced((value) => !value)}
          >
            {advanced ? "收起高级模式" : "打开高级模式"}
          </button>

          {advanced && (
            <div className="advanced-panel">
              <label>
                <span>渲染线程</span>
                <input
                  type="number"
                  min={1}
                  max={Math.max(env?.system.cpu_count ?? 8, 1)}
                  value={threads}
                  onChange={(event) => setThreads(Number(event.target.value))}
                />
              </label>
              <label>
                <span>起始端口</span>
                <input
                  type="number"
                  min={1024}
                  max={65535}
                  value={port}
                  onChange={(event) => setPort(Number(event.target.value))}
                />
              </label>
              <label className="checkbox-row">
                <input
                  type="checkbox"
                  checked={renderNether}
                  disabled={!inspection?.has_nether}
                  onChange={(event) => setRenderNether(event.target.checked)}
                />
                <span>渲染下界</span>
              </label>
              <label className="checkbox-row">
                <input
                  type="checkbox"
                  checked={renderEnd}
                  disabled={!inspection?.has_end}
                  onChange={(event) => setRenderEnd(event.target.checked)}
                />
                <span>渲染末地</span>
              </label>
              <button className="subtle-button" type="button" onClick={chooseOutputRoot}>
                选择输出目录
              </button>
              <PathBox value={outputRoot || "默认：世界目录旁的 TongCraftRenderOutput"} />
            </div>
          )}

          <div className="action-row">
            <button className="primary-button large" type="button" disabled={!canStart || busy} onClick={startRender}>
              开始渲染
            </button>
            <button className="danger-button" type="button" disabled={!status.process_running || busy} onClick={stopRender}>
              停止
            </button>
          </div>
        </aside>

        <section className="monitor-panel">
          <div className="progress-header">
            <div>
              <p>实时进度</p>
              <h2>{status.progress ? `${status.progress.map} ${progress.toFixed(2)}%` : statusLabel(status)}</h2>
            </div>
            <div className="eta-box">
              <span>预计剩余</span>
              <strong>{status.progress?.eta ?? "等待日志"}</strong>
            </div>
          </div>

          <div className="progress-track" aria-label="渲染进度">
            <div style={{ width: `${Math.max(0, Math.min(progress, 100))}%` }} />
          </div>

          <div className="metrics">
            <Metric label="运行时间" value={formatDuration(status.elapsed_seconds)} />
            <Metric label="输出大小" value={formatBytes(status.output_size_bytes)} />
            <Metric label="输出文件" value={`${status.output_file_count}`} />
            <Metric label="端口" value={status.port ? `${status.port}` : "未启动"} />
          </div>

          <div className="result-actions">
            <button type="button" className="subtle-button" disabled={!status.preview_url} onClick={openPreview}>
              打开预览
            </button>
            <button type="button" className="subtle-button" disabled={!status.output_dir} onClick={openOutput}>
              打开输出目录
            </button>
            <button type="button" className="subtle-button" disabled={!status.job_dir} onClick={exportDiagnostic}>
              导出诊断报告
            </button>
          </div>

          <div className="view-switcher" role="tablist" aria-label="查看模式">
            <button
              type="button"
              className={activeView === "preview" ? "active" : ""}
              onClick={() => setActiveView("preview")}
            >
              预览
            </button>
            <button
              type="button"
              className={activeView === "logs" ? "active" : ""}
              onClick={() => setActiveView("logs")}
            >
              日志
            </button>
          </div>

          <div className="viewer-panel">
            {activeView === "preview" ? (
              status.preview_url ? (
                <iframe title="BlueMap 本地预览" src={status.preview_url} />
              ) : (
                <div className="empty-preview">
                  <strong>预览等待启动</strong>
                  <span>开始渲染后，这里会直接加载本地 BlueMap。</span>
                </div>
              )
            ) : (
              <div className="log-panel">
                <div className="log-title">
                  <span>日志</span>
                  {status.error && <strong>{status.error}</strong>}
                </div>
                <pre>{status.logs.length ? status.logs.join("\n") : "等待 BlueMap 输出..."}</pre>
              </div>
            )}
          </div>
        </section>
      </section>
    </main>
  );
}

function CheckItem({ label, ok, value }: { label: string; ok: boolean; value: string }) {
  return (
    <div className="check-item">
      <span className={ok ? "ok" : "bad"}>{ok ? "通过" : "注意"}</span>
      <div>
        <strong>{label}</strong>
        <small>{value}</small>
      </div>
    </div>
  );
}

function PathBox({ value }: { value: string }) {
  return <div className="path-box" title={value}>{value}</div>;
}

function Metric({ label, value }: { label: string; value: string }) {
  return (
    <div className="metric">
      <span>{label}</span>
      <strong>{value}</strong>
    </div>
  );
}
