use std::path::{Path, PathBuf};
use std::process::Command;

use launcher_core::InstanceManifest;
use tauri::State;

use crate::error::AppError;
use crate::state::AppState;

#[tauri::command]
pub fn reveal_instance_workspace(state: State<'_, AppState>, id: String) -> Result<(), AppError> {
    let instance = InstanceManifest::get(&state.paths, &id)?;
    reveal_path(&PathBuf::from(instance.workspace))
}

#[tauri::command]
pub fn reveal_instance_config(state: State<'_, AppState>, id: String) -> Result<(), AppError> {
    let instance = InstanceManifest::get(&state.paths, &id)?;
    let workspace = PathBuf::from(instance.workspace);
    let profile = workspace
        .join("profiles")
        .join(&instance.profile)
        .join("cordis.patch.yml");
    if profile.exists() {
        reveal_path(&profile)
    } else {
        reveal_path(&workspace)
    }
}

fn reveal_path(path: &Path) -> Result<(), AppError> {
    #[cfg(target_os = "windows")]
    {
        let arg = if path.is_file() {
            format!("/select,{}", path.display())
        } else {
            path.display().to_string()
        };
        Command::new("explorer")
            .arg(arg)
            .spawn()
            .map_err(|e| AppError::msg(format!("open Explorer failed: {e}")))?;
        return Ok(());
    }

    #[cfg(target_os = "macos")]
    {
        Command::new("open")
            .arg(path)
            .spawn()
            .map_err(|e| AppError::msg(format!("open Finder failed: {e}")))?;
        return Ok(());
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        Command::new("xdg-open")
            .arg(path)
            .spawn()
            .map_err(|e| AppError::msg(format!("open file manager failed: {e}")))?;
        return Ok(());
    }
}
