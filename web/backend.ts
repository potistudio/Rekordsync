// Rust バックエンド（src-tauri/src/backend.rs）を呼び出すための薄いラッパー。

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export interface BackendError {
  code: "error" | "cancelled" | "busy" | "backend";
  message: string;
  traceback?: string;
}

export interface Status {
  app_name: string;
  version: string;
  db_found: boolean;
  db_path: string | null;
  db_name: string | null;
  package_ext: string;
  default_extract_dir: string;
}

export interface PlaylistRow {
  uuid: string;
  name: string;
  parent: string | null;
  is_folder: boolean;
  track_count: number;
}

export interface PackageInfo {
  path: string;
  name: string;
  track_count: number;
  playlists: PlaylistRow[];
}

export interface ExportResult {
  dest: string;
  playlists: number;
  tracks: number;
}

export interface ImportOptions {
  extract_dir: string;
  conflict_policy: "rename" | "merge" | "skip";
  import_cues: boolean;
  import_tags: boolean;
  import_analysis: boolean;
  import_artwork: boolean;
}

export interface ImportResult {
  summary: string;
  warnings: string[];
}

export interface Progress {
  message: string;
  current: number;
  total: number;
}

export function isBackendError(e: unknown): e is BackendError {
  return typeof e === "object" && e !== null && "code" in e && "message" in e;
}

function call<T>(method: string, params: Record<string, unknown> = {}): Promise<T> {
  return invoke<T>("backend_call", { method, params });
}

export const backend = {
  status: () => call<Status>("status"),
  listPlaylists: () => call<PlaylistRow[]>("list_playlists"),
  inspectPackage: (path: string) => call<PackageInfo>("inspect_package", { path }),
  exportPlaylists: (uuids: string[], dest: string) =>
    call<ExportResult>("export", { uuids, dest }),
  importPackage: (path: string, uuids: string[], options: ImportOptions) =>
    call<ImportResult>("import", { path, uuids, options }),
  cancel: () => call<null>("cancel"),
};

export function onProgress(handler: (p: Progress) => void): Promise<UnlistenFn> {
  return listen<Progress>("backend-progress", (e) => handler(e.payload));
}
