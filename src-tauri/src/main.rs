#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use chrono::{DateTime, Utc};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::{
    collections::VecDeque,
    fs::{self, File},
    io::{BufRead, BufReader, Read, Seek, SeekFrom, Write},
    net::TcpListener,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};
use sysinfo::System;
use tauri::{Manager, State};
use uuid::Uuid;

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

const MAX_LOG_LINES: usize = 500;
const DEFAULT_PORT_END: u16 = 8110;
#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x08000000;

#[derive(Default)]
struct AppState {
    inner: Arc<Mutex<RenderManager>>,
}

#[derive(Default)]
struct RenderManager {
    child: Option<Child>,
    job: Option<RenderJob>,
}

#[derive(Clone)]
struct RenderJob {
    id: String,
    world_path: PathBuf,
    job_dir: PathBuf,
    output_dir: PathBuf,
    preview_url: String,
    port: u16,
    started_at: DateTime<Utc>,
    completed_at: Option<DateTime<Utc>>,
    state: RenderState,
    progress: Option<ProgressInfo>,
    logs: VecDeque<String>,
    error: Option<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "lowercase")]
enum RenderState {
    Idle,
    Running,
    Completed,
    Failed,
    Stopped,
}

impl Default for RenderState {
    fn default() -> Self {
        Self::Idle
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct ProgressInfo {
    map: String,
    percent: f64,
    eta: String,
}

#[derive(Serialize)]
struct JavaInfo {
    found: bool,
    version: Option<String>,
    major: Option<u32>,
    path: Option<String>,
    output: String,
}

#[derive(Serialize)]
struct SystemInfo {
    os: String,
    arch: String,
    cpu_count: usize,
    recommended_threads: usize,
    total_memory_mb: Option<u64>,
}

#[derive(Serialize)]
struct EnvironmentInfo {
    java: JavaInfo,
    system: SystemInfo,
    bluemap_jar_found: bool,
    bluemap_jar_path: Option<String>,
}

#[derive(Serialize)]
struct WorldInspection {
    valid: bool,
    world_path: String,
    world_name: String,
    has_level_dat: bool,
    has_nether: bool,
    has_end: bool,
    size_bytes: u64,
    file_count: u64,
    estimated_required_bytes: u64,
    warnings: Vec<String>,
}

#[derive(Deserialize)]
struct StartRenderRequest {
    world_path: String,
    output_root: Option<String>,
    threads: usize,
    port: u16,
    render_nether: bool,
    render_end: bool,
}

#[derive(Serialize)]
struct RenderStatus {
    state: RenderState,
    job_id: Option<String>,
    world_path: Option<String>,
    job_dir: Option<String>,
    output_dir: Option<String>,
    preview_url: Option<String>,
    port: Option<u16>,
    started_at: Option<String>,
    completed_at: Option<String>,
    elapsed_seconds: u64,
    progress: Option<ProgressInfo>,
    output_size_bytes: u64,
    output_file_count: u64,
    process_running: bool,
    logs: Vec<String>,
    error: Option<String>,
}

#[derive(Serialize)]
struct DiagnosticReport {
    generated_at: String,
    environment: EnvironmentInfo,
    status: RenderStatus,
    world_inspection: Option<WorldInspection>,
}

#[tauri::command]
fn check_environment(app: tauri::AppHandle) -> EnvironmentInfo {
    let mut system = System::new_all();
    system.refresh_memory();
    let cpu_count = std::thread::available_parallelism()
        .map(|count| count.get())
        .unwrap_or(1);
    let recommended_threads = cpu_count.saturating_sub(1).clamp(1, 4);
    let jar = locate_bluemap_jar(&app);
    EnvironmentInfo {
        java: probe_java(),
        system: SystemInfo {
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
            cpu_count,
            recommended_threads,
            total_memory_mb: Some(system.total_memory() / 1024 / 1024),
        },
        bluemap_jar_found: jar.as_ref().is_some_and(|path| path.exists()),
        bluemap_jar_path: jar.map(|path| path.display().to_string()),
    }
}

#[tauri::command]
fn inspect_world(world_path: String) -> Result<WorldInspection, String> {
    inspect_world_path(Path::new(&world_path))
}

#[tauri::command]
fn start_render(
    app: tauri::AppHandle,
    state: State<AppState>,
    request: StartRenderRequest,
) -> Result<RenderStatus, String> {
    let java = probe_java();
    if !java.found || java.major.unwrap_or_default() < 21 {
        return Err("需要 Java 21 或更新版本。请先安装对应系统架构的 JDK。".to_string());
    }

    let jar = locate_bluemap_jar(&app)
        .filter(|path| path.exists())
        .ok_or_else(|| "找不到 bin/BlueMap-cli.jar，无法启动 BlueMap。".to_string())?;
    let java_jar = java_compatible_path(&jar);
    let world = PathBuf::from(&request.world_path);
    let inspection = inspect_world_path(&world)?;
    if !inspection.valid {
        return Err("选择的目录不是有效的 Minecraft Java 世界目录。".to_string());
    }

    {
        let manager = state.inner.lock().map_err(lock_error)?;
        if matches!(
            manager.job.as_ref().map(|job| &job.state),
            Some(RenderState::Running)
        ) {
            return Err("已有渲染任务正在运行。".to_string());
        }
    }

    let port = find_available_port(request.port)?;
    let job_id = Uuid::new_v4().to_string();
    let output_root = request
        .output_root
        .as_deref()
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            world
                .parent()
                .map(|path| path.join("TongCraftRenderOutput"))
                .unwrap_or_else(|| PathBuf::from("TongCraftRenderOutput"))
        });
    let job_dir = output_root.join(format!("render-{}", Utc::now().format("%Y%m%d-%H%M%S")));
    let output_dir = job_dir.join("web");
    prepare_job_directory(&job_dir, &world, port, request.threads, request.render_nether, request.render_end)?;

    let mut command = Command::new("java");
    command
        .args(["-jar"])
        .arg(&java_jar)
        .args(["-r", "-u", "-w"])
        .current_dir(&job_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    apply_hidden_window(&mut command);
    let mut child = command
        .spawn()
        .map_err(|err| format!("启动 BlueMap 失败：{err}"))?;

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let preview_url = format!("http://127.0.0.1:{port}");
    let job = RenderJob {
        id: job_id,
        world_path: world,
        job_dir: job_dir.clone(),
        output_dir: output_dir.clone(),
        preview_url,
        port,
        started_at: Utc::now(),
        completed_at: None,
        state: RenderState::Running,
        progress: None,
        logs: VecDeque::new(),
        error: None,
    };

    {
        let mut manager = state.inner.lock().map_err(lock_error)?;
        manager.child = Some(child);
        manager.job = Some(job);
    }

    if let Some(stdout) = stdout {
        spawn_line_reader(state.inner.clone(), stdout);
    }
    if let Some(stderr) = stderr {
        spawn_line_reader(state.inner.clone(), stderr);
    }
    spawn_log_monitor(state.inner.clone(), job_dir.join("data/logs/bluemap-debug.log"));
    spawn_exit_monitor(state.inner.clone());

    get_render_status(state)
}

#[tauri::command]
fn get_render_status(state: State<AppState>) -> Result<RenderStatus, String> {
    let manager = state.inner.lock().map_err(lock_error)?;
    Ok(status_from_manager(&manager))
}

#[tauri::command]
fn stop_render(state: State<AppState>) -> Result<RenderStatus, String> {
    let mut manager = state.inner.lock().map_err(lock_error)?;
    if let Some(child) = manager.child.as_mut() {
        let _ = child.kill();
        let _ = child.wait();
    }
    manager.child = None;
    if let Some(job) = manager.job.as_mut() {
        job.state = RenderState::Stopped;
        job.completed_at = Some(Utc::now());
        push_log(job, "渲染任务已停止。".to_string());
    }
    Ok(status_from_manager(&manager))
}

#[tauri::command]
fn open_preview(state: State<AppState>) -> Result<(), String> {
    let manager = state.inner.lock().map_err(lock_error)?;
    let url = manager
        .job
        .as_ref()
        .map(|job| job.preview_url.clone())
        .ok_or_else(|| "还没有可打开的预览地址。".to_string())?;
    open_external(&url)
}

#[tauri::command]
fn open_output_folder(state: State<AppState>) -> Result<(), String> {
    let manager = state.inner.lock().map_err(lock_error)?;
    let path = manager
        .job
        .as_ref()
        .map(|job| job.output_dir.clone())
        .ok_or_else(|| "还没有输出目录。".to_string())?;
    open_external(&path.display().to_string())
}

#[tauri::command]
fn export_diagnostic_report(app: tauri::AppHandle, state: State<AppState>) -> Result<String, String> {
    let manager = state.inner.lock().map_err(lock_error)?;
    let status = status_from_manager(&manager);
    let job = manager
        .job
        .as_ref()
        .ok_or_else(|| "没有可导出的渲染任务。".to_string())?;
    let inspection = inspect_world_path(&job.world_path).ok();
    let report = DiagnosticReport {
        generated_at: Utc::now().to_rfc3339(),
        environment: check_environment(app),
        status,
        world_inspection: inspection,
    };
    let report_path = job.job_dir.join("diagnostic-report.json");
    let json = serde_json::to_string_pretty(&report).map_err(|err| err.to_string())?;
    fs::write(&report_path, json).map_err(|err| format!("写入诊断报告失败：{err}"))?;
    Ok(report_path.display().to_string())
}

fn status_from_manager(manager: &RenderManager) -> RenderStatus {
    let Some(job) = manager.job.as_ref() else {
        return RenderStatus {
            state: RenderState::Idle,
            job_id: None,
            world_path: None,
            job_dir: None,
            output_dir: None,
            preview_url: None,
            port: None,
            started_at: None,
            completed_at: None,
            elapsed_seconds: 0,
            progress: None,
            output_size_bytes: 0,
            output_file_count: 0,
            process_running: false,
            logs: vec![],
            error: None,
        };
    };

    let (output_size_bytes, output_file_count) = dir_stats(&job.output_dir).unwrap_or((0, 0));
    let elapsed_seconds = Utc::now()
        .signed_duration_since(job.started_at)
        .num_seconds()
        .max(0) as u64;

    RenderStatus {
        state: job.state.clone(),
        job_id: Some(job.id.clone()),
        world_path: Some(job.world_path.display().to_string()),
        job_dir: Some(job.job_dir.display().to_string()),
        output_dir: Some(job.output_dir.display().to_string()),
        preview_url: Some(job.preview_url.clone()),
        port: Some(job.port),
        started_at: Some(job.started_at.to_rfc3339()),
        completed_at: job.completed_at.map(|time| time.to_rfc3339()),
        elapsed_seconds,
        progress: job.progress.clone(),
        output_size_bytes,
        output_file_count,
        process_running: manager.child.is_some(),
        logs: job.logs.iter().cloned().collect(),
        error: job.error.clone(),
    }
}

fn probe_java() -> JavaInfo {
    let output = Command::new("java").arg("-version").output();
    match output {
        Ok(output) => {
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            let version = parse_java_version(&text);
            JavaInfo {
                found: output.status.success() || version.is_some(),
                version: version.clone(),
                major: version.as_deref().and_then(java_major),
                path: Some("java".to_string()),
                output: text,
            }
        }
        Err(err) => JavaInfo {
            found: false,
            version: None,
            major: None,
            path: None,
            output: err.to_string(),
        },
    }
}

fn parse_java_version(text: &str) -> Option<String> {
    let re = Regex::new(r#""([0-9][^"]*)""#).ok()?;
    re.captures(text)
        .and_then(|captures| captures.get(1))
        .map(|value| value.as_str().to_string())
}

fn java_major(version: &str) -> Option<u32> {
    let mut parts = version.split('.');
    let first = parts.next()?.parse::<u32>().ok()?;
    if first == 1 {
        parts.next()?.parse::<u32>().ok()
    } else {
        Some(first)
    }
}

fn inspect_world_path(world: &Path) -> Result<WorldInspection, String> {
    let has_level_dat = world.join("level.dat").is_file();
    let has_nether = world.join("DIM-1").is_dir();
    let has_end = world.join("DIM1").is_dir();
    let (size_bytes, file_count) = dir_stats(world)?;
    let mut warnings = vec![];
    if !has_level_dat {
        warnings.push("这个目录缺少 level.dat，BlueMap 可能无法识别为 Java 世界。".to_string());
    }
    if !has_nether {
        warnings.push("未发现 DIM-1，下界渲染会自动关闭。".to_string());
    }
    if !has_end {
        warnings.push("未发现 DIM1，末地渲染会自动关闭。".to_string());
    }
    Ok(WorldInspection {
        valid: has_level_dat,
        world_path: world.display().to_string(),
        world_name: world
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("world")
            .to_string(),
        has_level_dat,
        has_nether,
        has_end,
        size_bytes,
        file_count,
        estimated_required_bytes: size_bytes.saturating_add(5 * 1024 * 1024 * 1024),
        warnings,
    })
}

fn dir_stats(path: &Path) -> Result<(u64, u64), String> {
    if !path.exists() {
        return Ok((0, 0));
    }
    let mut size = 0u64;
    let mut files = 0u64;
    let mut stack = vec![path.to_path_buf()];
    while let Some(current) = stack.pop() {
        let entries = match fs::read_dir(&current) {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let metadata = match entry.metadata() {
                Ok(metadata) => metadata,
                Err(_) => continue,
            };
            if metadata.is_dir() {
                stack.push(path);
            } else if metadata.is_file() {
                files += 1;
                size = size.saturating_add(metadata.len());
            }
        }
    }
    Ok((size, files))
}

fn locate_bluemap_jar(app: &tauri::AppHandle) -> Option<PathBuf> {
    if let Ok(resource_dir) = app.path().resource_dir() {
        let packaged = resource_dir.join("BlueMap-cli.jar");
        if packaged.exists() {
            return Some(packaged);
        }
    }
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let app_root = manifest_dir.parent()?;
    let local = app_root.join("bin/BlueMap-cli.jar");
    if local.exists() {
        return Some(local);
    }
    app_root
        .parent()
        .map(|repo_root| repo_root.join("bin/BlueMap-cli.jar"))
}

fn java_compatible_path(path: &Path) -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        let text = path.display().to_string();
        if let Some(stripped) = text.strip_prefix(r"\\?\") {
            return PathBuf::from(stripped);
        }
    }
    path.to_path_buf()
}

#[cfg(target_os = "windows")]
fn apply_hidden_window(command: &mut Command) {
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(target_os = "windows"))]
fn apply_hidden_window(_: &mut Command) {}

fn prepare_job_directory(
    job_dir: &Path,
    world: &Path,
    port: u16,
    threads: usize,
    render_nether: bool,
    render_end: bool,
) -> Result<(), String> {
    for dir in [
        "config/maps",
        "config/storages",
        "data/logs",
        "web/css",
        "web/js",
    ] {
        fs::create_dir_all(job_dir.join(dir)).map_err(|err| format!("创建工作目录失败：{err}"))?;
    }

    let world_path = normalize_path(world);
    write_file(
        &job_dir.join("config/core.conf"),
        &format!(
            r#"accept-download: true
data: "data"
render-thread-count: {}
scan-for-mod-resources: true
metrics: false
log: {{
  file: "data/logs/bluemap-debug.log"
  append: false
}}
"#,
            threads.clamp(1, 64)
        ),
    )?;
    write_file(
        &job_dir.join("config/webserver.conf"),
        &format!(
            r#"enabled: true
webroot: "web"
port: {}
log: {{
  file: "data/logs/webserver.log"
  append: false
  format: "%1$s \"%3$s %4$s %5$s\" %6$s %7$s"
}}
"#,
            port
        ),
    )?;
    write_file(
        &job_dir.join("config/webapp.conf"),
        r#"enabled: true
webroot: "web"
use-cookies: true
"#,
    )?;
    write_file(
        &job_dir.join("config/storages/file.conf"),
        r#"storage-type: file
root: "web/maps"
compression: gzip
"#,
    )?;
    write_file(
        &job_dir.join("config/maps/world.conf"),
        &format!(
            r##"world: "{}"
dimension: "minecraft:overworld"
name: "TongCraft Overworld"
sorting: 0
start-pos: {{ x: 0, z: 0 }}
sky-color: "#7dabff"
void-color: "#030712"
ambient-light: 0
remove-caves-below-y: 55
enable-perspective-view: false
enable-flat-view: true
enable-free-flight-view: false
enable-hires: false
storage: "file"
"##,
            hocon_escape(&world_path)
        ),
    )?;

    if render_nether && world.join("DIM-1").is_dir() {
        write_file(
            &job_dir.join("config/maps/world_nether.conf"),
            &format!(
                r##"world: "{}/DIM-1"
dimension: "minecraft:the_nether"
name: "TongCraft Nether"
sorting: 10
start-pos: {{ x: 0, z: 0 }}
sky-color: "#1a0908"
void-color: "#050203"
ambient-light: 0.1
remove-caves-below-y: 0
enable-perspective-view: false
enable-flat-view: true
enable-free-flight-view: false
enable-hires: false
storage: "file"
"##,
                hocon_escape(&world_path)
            ),
        )?;
    }

    if render_end && world.join("DIM1").is_dir() {
        write_file(
            &job_dir.join("config/maps/world_the_end.conf"),
            &format!(
                r##"world: "{}/DIM1"
dimension: "minecraft:the_end"
name: "TongCraft End"
sorting: 20
start-pos: {{ x: 0, z: 0 }}
sky-color: "#1f1636"
void-color: "#020109"
ambient-light: 0.15
remove-caves-below-y: 0
enable-perspective-view: false
enable-flat-view: true
enable-free-flight-view: false
enable-hires: false
storage: "file"
"##,
                hocon_escape(&world_path)
            ),
        )?;
    }

    copy_optional_customizations(job_dir);
    Ok(())
}

fn write_file(path: &Path, content: &str) -> Result<(), String> {
    let mut file = File::create(path).map_err(|err| format!("写入 {} 失败：{err}", path.display()))?;
    file.write_all(content.as_bytes())
        .map_err(|err| format!("写入 {} 失败：{err}", path.display()))
}

fn normalize_path(path: &Path) -> String {
    path.display().to_string().replace('\\', "/")
}

fn hocon_escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn copy_optional_customizations(job_dir: &Path) {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let Some(repo_root) = manifest_dir.parent().and_then(|path| path.parent()) else {
        return;
    };
    for folder in ["css", "js"] {
        let source = repo_root.join("web").join(folder);
        let target = job_dir.join("web").join(folder);
        let _ = copy_dir_contents(&source, &target);
    }
}

fn copy_dir_contents(source: &Path, target: &Path) -> Result<(), String> {
    if !source.exists() {
        return Ok(());
    }
    fs::create_dir_all(target).map_err(|err| err.to_string())?;
    for entry in fs::read_dir(source).map_err(|err| err.to_string())?.flatten() {
        let source_path = entry.path();
        let target_path = target.join(entry.file_name());
        if source_path.is_dir() {
            copy_dir_contents(&source_path, &target_path)?;
        } else {
            let _ = fs::copy(&source_path, &target_path);
        }
    }
    Ok(())
}

fn find_available_port(start: u16) -> Result<u16, String> {
    let end = start.saturating_add(DEFAULT_PORT_END.saturating_sub(8100));
    for port in start..=end {
        if TcpListener::bind(("127.0.0.1", port)).is_ok() {
            return Ok(port);
        }
    }
    Err(format!("端口 {start}-{end} 都不可用。"))
}

fn spawn_line_reader<R>(state: Arc<Mutex<RenderManager>>, reader: R)
where
    R: Read + Send + 'static,
{
    thread::spawn(move || {
        let reader = BufReader::new(reader);
        for line in reader.lines().map_while(Result::ok) {
            handle_log_line(&state, line);
        }
    });
}

fn spawn_log_monitor(state: Arc<Mutex<RenderManager>>, log_path: PathBuf) {
    thread::spawn(move || {
        let mut offset = 0u64;
        loop {
            let should_continue = {
                let manager = match state.lock() {
                    Ok(manager) => manager,
                    Err(_) => return,
                };
                matches!(
                    manager.job.as_ref().map(|job| &job.state),
                    Some(RenderState::Running | RenderState::Completed)
                )
            };
            if !should_continue {
                return;
            }

            if let Ok(mut file) = File::open(&log_path) {
                if file.seek(SeekFrom::Start(offset)).is_ok() {
                    let mut text = String::new();
                    if file.read_to_string(&mut text).is_ok() {
                        offset = file.stream_position().unwrap_or(offset);
                        for line in text.lines() {
                            handle_log_line(&state, line.to_string());
                        }
                    }
                }
            }
            thread::sleep(Duration::from_millis(1200));
        }
    });
}

fn spawn_exit_monitor(state: Arc<Mutex<RenderManager>>) {
    thread::spawn(move || loop {
        thread::sleep(Duration::from_secs(1));
        let mut manager = match state.lock() {
            Ok(manager) => manager,
            Err(_) => return,
        };
        let Some(child) = manager.child.as_mut() else {
            return;
        };
        match child.try_wait() {
            Ok(Some(exit)) => {
                drop(manager);
                thread::sleep(Duration::from_millis(250));
                let mut manager = match state.lock() {
                    Ok(manager) => manager,
                    Err(_) => return,
                };
                manager.child = None;
                if let Some(job) = manager.job.as_mut() {
                    if !matches!(job.state, RenderState::Completed | RenderState::Stopped) {
                        job.state = if exit.success() {
                            RenderState::Completed
                        } else {
                            RenderState::Failed
                        };
                        job.completed_at = Some(Utc::now());
                        if !exit.success() {
                            job.error = Some(format!("BlueMap 进程退出：{exit}"));
                        }
                    }
                }
                return;
            }
            Ok(None) => {}
            Err(err) => {
                if let Some(job) = manager.job.as_mut() {
                    job.state = RenderState::Failed;
                    job.completed_at = Some(Utc::now());
                    job.error = Some(err.to_string());
                }
                return;
            }
        }
    });
}

fn handle_log_line(state: &Arc<Mutex<RenderManager>>, line: String) {
    let mut manager = match state.lock() {
        Ok(manager) => manager,
        Err(_) => return,
    };
    let Some(job) = manager.job.as_mut() else {
        return;
    };
    if line.trim().is_empty() {
        return;
    }
    if let Some(progress) = parse_progress(&line) {
        job.progress = Some(progress);
        job.state = RenderState::Running;
    }
    if line.contains("Your maps are now all up-to-date") {
        job.state = RenderState::Completed;
        job.completed_at = Some(Utc::now());
    }
    if is_fatal_log_line(&line) {
        job.error = Some(line.clone());
    }
    push_log(job, line);
}

fn parse_progress(line: &str) -> Option<ProgressInfo> {
    let re = Regex::new(r#"updating map '([^']+)': ([0-9.]+)% \(ETA: ([^)]+)\)"#).ok()?;
    let captures = re.captures(line)?;
    Some(ProgressInfo {
        map: captures.get(1)?.as_str().to_string(),
        percent: captures.get(2)?.as_str().parse().ok()?,
        eta: captures.get(3)?.as_str().to_string(),
    })
}

fn is_fatal_log_line(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    if lower.contains("failed to load resource") {
        return false;
    }
    lower.contains("[error]")
        || lower.contains(" error]")
        || lower.contains("exception in thread")
        || lower.contains("fatal")
}

fn push_log(job: &mut RenderJob, line: String) {
    if job.logs.back().is_some_and(|last| last == &line) {
        return;
    }
    job.logs.push_back(line);
    while job.logs.len() > MAX_LOG_LINES {
        job.logs.pop_front();
    }
}

fn open_external(target: &str) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    let status = Command::new("cmd")
        .args(["/C", "start", "", target])
        .status();

    #[cfg(target_os = "macos")]
    let status = Command::new("open").arg(target).status();

    #[cfg(all(unix, not(target_os = "macos")))]
    let status = Command::new("xdg-open").arg(target).status();

    status
        .map_err(|err| format!("打开失败：{err}"))
        .and_then(|status| {
            if status.success() {
                Ok(())
            } else {
                Err(format!("打开命令退出：{status}"))
            }
        })
}

fn lock_error<T>(_: std::sync::PoisonError<T>) -> String {
    "内部状态锁定失败。".to_string()
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            check_environment,
            inspect_world,
            start_render,
            get_render_status,
            stop_render,
            open_preview,
            open_output_folder,
            export_diagnostic_report
        ])
        .run(tauri::generate_context!())
        .expect("error while running TongCraft BlueMap Renderer");
}
