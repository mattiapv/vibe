use std::{
    collections::HashSet,
    fs,
    path::Path,
    process::Command,
};

use serde::{Deserialize, Serialize};

const REGISTRY_FILE_NAME: &str = "vm_registry.json";

#[derive(Clone, Serialize, Deserialize)]
pub struct VmRecord {
    pub folder_path: String,
    pub created_at: String,
}

#[derive(Serialize, Deserialize, Default)]
struct Registry {
    vm_folders: Vec<VmRecord>,
}

pub fn record_vm_launch(
    cache_dir: &Path,
    folder_path: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut registry = load_registry(cache_dir)?;
    let folder = folder_path.canonicalize()?.to_string_lossy().to_string();

    if registry.vm_folders.iter().any(|r| r.folder_path == folder) {
        return Ok(());
    }

    registry.vm_folders.push(VmRecord {
        folder_path: folder,
        created_at: current_date_yyyy_mm_dd()?,
    });
    save_registry(cache_dir, &registry)?;
    Ok(())
}

pub fn list_vm_records(cache_dir: &Path) -> Result<Vec<VmRecord>, Box<dyn std::error::Error>> {
    let mut records = load_registry(cache_dir)?.vm_folders;
    records.sort_by(|a, b| {
        b.created_at
            .cmp(&a.created_at)
            .then(a.folder_path.cmp(&b.folder_path))
    });
    Ok(records)
}

pub fn delete_vm_records(
    cache_dir: &Path,
    folders: &[String],
) -> Result<(), Box<dyn std::error::Error>> {
    if folders.is_empty() {
        return Ok(());
    }

    let to_delete: HashSet<&str> = folders.iter().map(String::as_str).collect();
    let mut registry = load_registry(cache_dir)?;
    registry
        .vm_folders
        .retain(|r| !to_delete.contains(r.folder_path.as_str()));
    save_registry(cache_dir, &registry)?;
    Ok(())
}

fn registry_path(cache_dir: &Path) -> std::path::PathBuf {
    cache_dir.join(REGISTRY_FILE_NAME)
}

fn load_registry(cache_dir: &Path) -> Result<Registry, Box<dyn std::error::Error>> {
    let path = registry_path(cache_dir);
    if !path.exists() {
        return Ok(Registry::default());
    }

    let content = fs::read_to_string(&path)?;
    if content.trim().is_empty() {
        return Ok(Registry::default());
    }

    let registry: Registry = serde_json::from_str(&content)
        .map_err(|err| format!("Failed to parse registry JSON at {}: {err}", path.display()))?;
    Ok(registry)
}

fn save_registry(cache_dir: &Path, registry: &Registry) -> Result<(), Box<dyn std::error::Error>> {
    let path = registry_path(cache_dir);
    let tmp = path.with_extension("json.tmp");
    let json = serde_json::to_string_pretty(registry)?;
    fs::write(&tmp, format!("{json}\n"))?;
    fs::rename(tmp, path)?;
    Ok(())
}

fn current_date_yyyy_mm_dd() -> Result<String, Box<dyn std::error::Error>> {
    let output = Command::new("date").args(["-u", "+%F"]).output()?;
    if !output.status.success() {
        return Err("Failed to run `date -u +%F`".into());
    }
    let value = String::from_utf8(output.stdout)?.trim().to_string();
    if value.len() != 10 {
        return Err(format!("Unexpected date format from `date`: {value}").into());
    }
    Ok(value)
}
