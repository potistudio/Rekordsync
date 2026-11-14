// リリースビルドでコンソールウィンドウを出さない。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod backend;
mod config;
mod exporter;
mod importer;
mod models;
mod package;
#[cfg(test)]
mod real_db_tests;
mod rekordbox;
mod task;

use backend::{Backend, BackendError};
use serde_json::Value;
use tauri::{AppHandle, Manager, RunEvent, State};

/// バックエンドのメソッドを呼び出す。
#[tauri::command]
async fn backend_call(
    app: AppHandle,
    backend: State<'_, Backend>,
    method: String,
    params: Option<Value>,
) -> Result<Value, BackendError> {
    backend
        .call(&app, &method, params.unwrap_or(Value::Null))
        .await
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(Backend::default())
        .invoke_handler(tauri::generate_handler![backend_call])
        .build(tauri::generate_context!())
        .expect("failed to build tauri application")
        .run(|app, event| {
            if let RunEvent::Exit = event {
                app.state::<Backend>().shutdown();
            }
        });
}
