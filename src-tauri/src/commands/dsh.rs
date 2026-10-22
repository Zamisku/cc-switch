//! DeepSeek Harness 相关命令。

use tauri::State;

use crate::store::AppState;

/// 从 `$DSH_HOME/profiles/<profile>/cordis.patch.yml` 导入现有路由。
#[tauri::command]
pub fn import_dsh_providers_from_live(state: State<'_, AppState>) -> Result<usize, String> {
    crate::services::provider::import_dsh_providers_from_live(state.inner())
        .map_err(|error| error.to_string())
}

/// 当前 DSH 数据目录（界面显示与排错用）。
#[tauri::command]
pub fn get_dsh_home() -> String {
    crate::dsh_config::DshPaths::resolve()
        .root()
        .to_string_lossy()
        .into_owned()
}
