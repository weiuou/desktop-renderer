export type JavaInfo = {
  found: boolean;
  version: string | null;
  major: number | null;
  path: string | null;
  output: string;
};

export type SystemInfo = {
  os: string;
  arch: string;
  cpu_count: number;
  recommended_threads: number;
  total_memory_mb: number | null;
};

export type EnvironmentInfo = {
  java: JavaInfo;
  system: SystemInfo;
  bluemap_jar_found: boolean;
  bluemap_jar_path: string | null;
};

export type WorldInspection = {
  valid: boolean;
  world_path: string;
  world_name: string;
  has_level_dat: boolean;
  has_nether: boolean;
  has_end: boolean;
  size_bytes: number;
  file_count: number;
  estimated_required_bytes: number;
  warnings: string[];
};

export type ProgressInfo = {
  map: string;
  percent: number;
  eta: string;
};

export type RenderStatus = {
  state: "idle" | "running" | "completed" | "failed" | "stopped";
  job_id: string | null;
  world_path: string | null;
  job_dir: string | null;
  output_dir: string | null;
  preview_url: string | null;
  port: number | null;
  started_at: string | null;
  completed_at: string | null;
  elapsed_seconds: number;
  progress: ProgressInfo | null;
  output_size_bytes: number;
  output_file_count: number;
  process_running: boolean;
  logs: string[];
  error: string | null;
};

export type StartRenderRequest = {
  world_path: string;
  output_root: string | null;
  threads: number;
  port: number;
  render_nether: boolean;
  render_end: boolean;
};
