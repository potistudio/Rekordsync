import { getCurrentWindow } from "@tauri-apps/api/window";
import { ask, message, open, save } from "@tauri-apps/plugin-dialog";

import {
  backend,
  isBackendError,
  onProgress,
  type ImportOptions,
  type PackageInfo,
  type Status,
} from "./backend";
import { CheckTree } from "./tree";

import "@fontsource/wdxl-lubrifont-jp-n/400.css";

const APP = "rekordsync";

const $ = <T extends HTMLElement = HTMLElement>(id: string) =>
  document.getElementById(id) as T;

// ---------------------------------------------------------------------------
// 状態
// ---------------------------------------------------------------------------

let status: Status | null = null;
let busy = false;
let pkg: PackageInfo | null = null;

const exportTree = new CheckTree($("export-tree"), () => updateUi());
const importTree = new CheckTree($("import-tree"), () => updateUi());

// ---------------------------------------------------------------------------
// ステータスバー・ジョブ実行
// ---------------------------------------------------------------------------

const progressEl = $("progress");
const progressBar = $("progress-bar");
const statusEl = $("status");
const cancelBtn = $<HTMLButtonElement>("cancel");

function setStatus(text: string): void {
  statusEl.textContent = text;
  statusEl.title = text;
}

function setProgress(current: number, total: number): void {
  if (total > 0) {
    progressEl.classList.remove("indeterminate");
    progressBar.style.width = `${Math.min(100, (current / total) * 100)}%`;
  } else {
    progressEl.classList.add("indeterminate");
    progressBar.style.width = "";
  }
}

function resetProgress(): void {
  progressEl.classList.remove("indeterminate");
  progressBar.style.width = "0%";
}

onProgress((p) => {
  if (!busy) return;
  const suffix = p.total > 0 ? `（${p.current}/${p.total}）` : "";
  setStatus(`${p.message}${suffix}`);
  setProgress(p.current, p.total);
});

interface JobOptions {
  cancellable?: boolean;
  onCancelled?: () => unknown;
}

/** バックエンド処理を 1 つ実行する。実行中は操作を止め、失敗はダイアログで知らせる。 */
async function runJob<T>(
  busyMessage: string,
  task: () => Promise<T>,
  opts: JobOptions = {},
): Promise<T | undefined> {
  if (busy) {
    await message("別の処理が実行中です。お待ちください。", { title: APP });
    return undefined;
  }
  busy = true;
  setStatus(busyMessage);
  setProgress(0, 0);
  cancelBtn.hidden = !opts.cancellable;
  cancelBtn.disabled = false;
  updateUi();
  try {
    const result = await task();
    setStatus("準備完了");
    return result;
  } catch (e) {
    if (isBackendError(e) && e.code === "cancelled") {
      setStatus("キャンセルしました");
      await opts.onCancelled?.();
    } else {
      setStatus("エラー");
      const text = isBackendError(e) ? e.message : String(e);
      if (isBackendError(e) && e.traceback) console.error(e.traceback);
      await message(`エラーが発生しました:\n\n${text}`, { title: APP, kind: "error" });
    }
    return undefined;
  } finally {
    busy = false;
    cancelBtn.hidden = true;
    resetProgress();
    updateUi();
  }
}

cancelBtn.addEventListener("click", async () => {
  cancelBtn.disabled = true;
  setStatus("キャンセル中…");
  try {
    await backend.cancel();
  } catch (e) {
    console.error(e);
  }
});

// ---------------------------------------------------------------------------
// ボタンの有効・無効
// ---------------------------------------------------------------------------

function updateUi(): void {
  const db = status?.db_found ?? false;
  $<HTMLButtonElement>("db-refresh").disabled = busy;

  $<HTMLButtonElement>("export-load").disabled = busy || !db;
  $<HTMLButtonElement>("export-all").disabled = busy || exportTree.size === 0;
  $<HTMLButtonElement>("export-none").disabled = busy || exportTree.size === 0;
  $<HTMLButtonElement>("export-run").disabled =
    busy || !db || exportTree.checkedPlaylistCount() === 0;
  $("export-count").textContent =
    exportTree.size > 0 ? `SEL ${String(exportTree.checkedPlaylistCount()).padStart(3, "0")} / ${String(exportTree.size).padStart(3, "0")}` : "";

  $<HTMLButtonElement>("import-open").disabled = busy;
  $<HTMLButtonElement>("import-all").disabled = busy || importTree.size === 0;
  $<HTMLButtonElement>("import-none").disabled = busy || importTree.size === 0;
  $<HTMLButtonElement>("import-run").disabled =
    busy || !db || pkg === null || importTree.checkedPlaylistCount() === 0;
  $<HTMLButtonElement>("extract-browse").disabled = busy;
}

// ---------------------------------------------------------------------------
// DB 状態
// ---------------------------------------------------------------------------

async function refreshStatus(): Promise<void> {
  const pill = $("db-status");
  const pillText = $("db-status-text");
  pill.dataset.state = "pending";
  pillText.textContent = "SCANNING…";
  try {
    status = await backend.status();
    $("app-version").textContent = status.version;
    const extract = $<HTMLInputElement>("extract-dir");
    if (!extract.value) extract.value = status.default_extract_dir;
    if (status.db_found) {
      pill.dataset.state = "ok";
      pillText.textContent = `DB:ONLINE / ${status.db_name}`;
      pill.title = `データベースを検出しました
${status.db_path ?? ""}`;
    } else {
      pill.dataset.state = "ng";
      pillText.textContent = "DB:NOT FOUND";
      pill.title = "データベースが見つかりません。Rekordbox がインストール・初期化されているか確認してください。";
    }
  } catch (e) {
    status = null;
    pill.dataset.state = "ng";
    pillText.textContent = "BACKEND:OFFLINE";
    pill.title = isBackendError(e) ? e.message : String(e);
    setStatus(`バックエンドに接続できません: ${pill.title}`);
  }
  updateUi();
}

$("db-refresh").addEventListener("click", refreshStatus);

// ---------------------------------------------------------------------------
// タブ
// ---------------------------------------------------------------------------

document.querySelectorAll<HTMLButtonElement>(".tab").forEach((tab) => {
  tab.addEventListener("click", () => {
    const name = tab.dataset.tab;
    document.querySelectorAll<HTMLButtonElement>(".tab").forEach((t) => {
      t.setAttribute("aria-selected", String(t === tab));
    });
    $("panel-export").hidden = name !== "export";
    $("panel-import").hidden = name !== "import";
  });
});

// ---------------------------------------------------------------------------
// エクスポート
// ---------------------------------------------------------------------------

$("export-load").addEventListener("click", async () => {
  const rows = await runJob("プレイリストを読み込み中…", backend.listPlaylists);
  if (!rows) return;
  exportTree.setRows(rows, false, "プレイリストがありません。");
  setStatus(`${rows.length} 件のプレイリストを読み込みました`);
  updateUi();
});

$("export-all").addEventListener("click", () => exportTree.setAll(true));
$("export-none").addEventListener("click", () => exportTree.setAll(false));

$("export-run").addEventListener("click", async () => {
  const uuids = exportTree.checkedUuids();
  if (uuids.length === 0) return;
  const ext = status?.package_ext ?? ".rsync";
  const dest = await save({
    title: "パッケージの保存先",
    defaultPath: `playlists${ext}`,
    filters: [
      { name: "rekordsync パッケージ", extensions: [ext.slice(1)] },
      { name: "すべて", extensions: ["*"] },
    ],
  });
  if (!dest) return;

  const result = await runJob("エクスポート中…", () => backend.exportPlaylists(uuids, dest), {
    cancellable: true,
    onCancelled: () =>
      message("エクスポートをキャンセルしました。\n書きかけのパッケージは削除されました。", {
        title: APP,
      }),
  });
  if (!result) return;
  setStatus("エクスポート完了");
  await message(
    `エクスポート完了！\n\nプレイリスト ${result.playlists} 件 / 楽曲 ${result.tracks} 曲\n→ ${result.dest}`,
    { title: APP },
  );
});

// ---------------------------------------------------------------------------
// インポート
// ---------------------------------------------------------------------------

$("import-open").addEventListener("click", async () => {
  const ext = status?.package_ext ?? ".rsync";
  const path = await open({
    title: "パッケージを開く",
    multiple: false,
    directory: false,
    filters: [
      { name: "rekordsync パッケージ", extensions: [ext.slice(1)] },
      { name: "ZIP", extensions: ["zip"] },
      { name: "すべて", extensions: ["*"] },
    ],
  });
  if (typeof path !== "string") return;

  const info = await runJob("パッケージを解析中…", () => backend.inspectPackage(path));
  if (!info) return;
  pkg = info;
  const pkgLabel = $("import-pkg");
  pkgLabel.textContent = `${info.name}（プレイリスト ${info.playlists.length} / 楽曲 ${info.track_count}）`;
  pkgLabel.title = info.path;
  importTree.setRows(info.playlists, true, "このパッケージにはプレイリストがありません。");
  setStatus("パッケージを読み込みました");
  updateUi();
});

$("import-all").addEventListener("click", () => importTree.setAll(true));
$("import-none").addEventListener("click", () => importTree.setAll(false));

$("extract-browse").addEventListener("click", async () => {
  const input = $<HTMLInputElement>("extract-dir");
  const dir = await open({
    title: "楽曲の展開先フォルダ",
    directory: true,
    multiple: false,
    defaultPath: input.value || undefined,
  });
  if (typeof dir === "string") input.value = dir;
});

function readImportOptions(): ImportOptions {
  const checked = (id: string) => $<HTMLInputElement>(id).checked;
  const conflict =
    document.querySelector<HTMLInputElement>('input[name="conflict"]:checked')?.value ?? "rename";
  return {
    extract_dir: $<HTMLInputElement>("extract-dir").value.trim(),
    conflict_policy: conflict as ImportOptions["conflict_policy"],
    import_cues: checked("opt-cues"),
    import_tags: checked("opt-tags"),
    import_analysis: checked("opt-analysis"),
    import_artwork: checked("opt-artwork"),
  };
}

$("import-run").addEventListener("click", async () => {
  if (!pkg) return;
  const uuids = importTree.checkedUuids();
  if (uuids.length === 0) return;

  const ok = await ask(
    "ローカルの Rekordbox データベースに書き込みます。\n" +
      "事前に Rekordbox を終了し、バックアップを取ることを推奨します。\n\n続行しますか？",
    { title: APP, kind: "warning", okLabel: "取り込む", cancelLabel: "やめる" },
  );
  if (!ok) return;

  const path = pkg.path;
  const options = readImportOptions();
  const result = await runJob("取り込み中…", () => backend.importPackage(path, uuids, options));
  if (!result) return;
  setStatus("取り込み完了");
  await message(`取り込みが完了しました！\n\n${result.summary}`, { title: APP });
  if (result.warnings.length > 0) showWarnings(result.warnings);
});

function showWarnings(warnings: string[]): void {
  const dialog = $<HTMLDialogElement>("warnings");
  $("warnings-title").textContent = `警告 ${warnings.length} 件`;
  const list = $("warnings-list");
  list.replaceChildren(
    ...warnings.map((w) => {
      const li = document.createElement("li");
      li.textContent = w;
      return li;
    }),
  );
  dialog.showModal();
}

// ---------------------------------------------------------------------------
// 終了時の確認
// ---------------------------------------------------------------------------

getCurrentWindow().onCloseRequested(async (event) => {
  if (!busy) return;
  const ok = await ask("処理の途中です。終了すると処理が中断されます。終了しますか？", {
    title: APP,
    kind: "warning",
    okLabel: "終了する",
    cancelLabel: "戻る",
  });
  if (!ok) event.preventDefault();
});

// ---------------------------------------------------------------------------
// 起動
// ---------------------------------------------------------------------------

updateUi();
refreshStatus();
