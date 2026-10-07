use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs;
use std::path::{Component, Path, PathBuf};
#[cfg(target_os = "linux")]
use std::process::Command;
#[cfg(target_os = "windows")]
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_READ_BYTES: u64 = 200_000;
const MAX_CREATED_BYTES: usize = 100_000;
const MAX_EDIT_BYTES: u64 = 1_000_000;
const MAX_LIST_ENTRIES: usize = 100;
const MAX_PREVIEW_CHARS: usize = 2_000;

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationAction {
    pub name: String,
    pub arguments: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
    #[serde(skip)]
    pub preview_hash: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct AppLaunchTarget {
    id: String,
    label: String,
    kind: String,
    target: String,
}

pub fn supports(name: &str) -> bool {
    matches!(
        name,
        "list_directory"
            | "read_file"
            | "open_path"
            | "open_app"
            | "create_directory"
            | "create_file"
            | "move_file"
            | "write_file"
            | "erase_file_content"
    )
}

pub fn select_app_target(
    action: &mut AutomationAction,
    selected_id: Option<&str>,
) -> Result<(), String> {
    if action.name != "open_app" {
        return if selected_id.is_some() {
            Err("An app selection was supplied for a non-app action.".into())
        } else {
            Ok(())
        };
    }
    let selected = selected_id
        .ok_or_else(|| "Select the exact installed app before approving.".to_string())?;
    let arguments = action
        .arguments
        .as_object_mut()
        .ok_or_else(|| "The pending app request is invalid.".to_string())?;
    if let Some(choices) = arguments.get("appChoices").and_then(Value::as_array) {
        let target = choices
            .iter()
            .find(|candidate| candidate.get("id").and_then(Value::as_str) == Some(selected))
            .cloned()
            .ok_or_else(|| {
                "That app is not one of the requested exact-name matches.".to_string()
            })?;
        arguments.insert("launchTarget".into(), target);
        arguments.remove("appChoices");
    } else if arguments
        .get("launchTarget")
        .and_then(|target| target.get("id"))
        .and_then(Value::as_str)
        != Some(selected)
    {
        return Err("That app selection does not match the pending request.".into());
    }
    Ok(())
}

pub fn explicit_open_app(query: &str) -> Option<String> {
    let query = query.trim().trim_end_matches(['.', '!', '?']).trim();
    let lower = query.to_ascii_lowercase();
    let rest = [
        "please open ",
        "can you open ",
        "could you open ",
        "open ",
        "launch ",
        "start ",
    ]
    .iter()
    .find_map(|prefix| lower.strip_prefix(prefix).map(|_| &query[prefix.len()..]))?;
    let mut name = rest.trim();
    if name.to_ascii_lowercase().starts_with("the ") {
        name = name[4..].trim();
    }
    for suffix in [" for me", " please"] {
        if name.to_ascii_lowercase().ends_with(suffix) {
            name = name[..name.len() - suffix.len()].trim();
            break;
        }
    }
    for suffix in [" application", " app"] {
        if name.to_ascii_lowercase().ends_with(suffix) {
            name = name[..name.len() - suffix.len()].trim();
            break;
        }
    }
    if name.is_empty()
        || name.len() > 100
        || name.contains(['\\', '/', ':'])
        || Path::new(name).extension().is_some()
        || [" folder", " directory", " file", " document"]
            .iter()
            .any(|suffix| name.to_ascii_lowercase().ends_with(suffix))
        || name.split_whitespace().count() > 8
    {
        return None;
    }
    Some(name.to_string())
}

pub fn explicit_open_path(query: &str) -> Option<String> {
    let query = query.trim().trim_end_matches(['.', '!', '?']).trim();
    let lower = query.to_ascii_lowercase();
    let rest = ["open ", "show ", "browse to "]
        .iter()
        .find_map(|prefix| lower.strip_prefix(prefix).map(|_| &query[prefix.len()..]))?
        .trim();
    let mut path = rest;
    if path.to_ascii_lowercase().starts_with("the ") {
        path = path[4..].trim();
    }
    let has_explicit_type = [" folder", " directory", " file", " document"]
        .iter()
        .any(|suffix| path.to_ascii_lowercase().ends_with(suffix));
    for suffix in [" folder", " directory", " file", " document"] {
        if path.to_ascii_lowercase().ends_with(suffix) {
            path = path[..path.len() - suffix.len()].trim();
            break;
        }
    }
    if path.is_empty()
        || path.len() > 1024
        || path.contains('"')
        || !(has_explicit_type
            || path.contains(['\\', '/'])
            || path.starts_with('.')
            || Path::new(path).extension().is_some())
    {
        return None;
    }
    Some(path.to_string())
}

pub fn prepare(
    mut action: AutomationAction,
    folders: &[String],
) -> Result<AutomationAction, String> {
    if action.name == "open_app" {
        let app_name = action
            .arguments
            .get("appName")
            .and_then(Value::as_str)
            .filter(|name| !name.trim().is_empty())
            .ok_or_else(|| "The app name is missing.".to_string())?;
        let targets = find_app_targets(app_name)?;
        if targets.is_empty() {
            return Err(format!(
                "I couldn't find an installed app with the exact full name “{app_name}”. Say its full name as shown in the Windows Start menu."
            ));
        }
        let arguments = action
            .arguments
            .as_object_mut()
            .ok_or_else(|| "The app request has invalid arguments.".to_string())?;
        arguments.remove("launchTarget");
        arguments.remove("appChoices");
        if targets.len() == 1 {
            arguments.insert(
                "launchTarget".into(),
                serde_json::to_value(&targets[0])
                    .map_err(|error| format!("Could not prepare app launch: {error}"))?,
            );
        } else {
            arguments.insert(
                "appChoices".into(),
                serde_json::to_value(&targets)
                    .map_err(|error| format!("Could not prepare app choices: {error}"))?,
            );
        }
        return Ok(action);
    }

    let roots = authorized_roots(folders)?;
    match action.name.as_str() {
        "open_path" | "list_directory" => {
            let path = action_path(&action.arguments, "path", &roots, true)?;
            let metadata = fs::metadata(&path)
                .map_err(|e| format!("Cannot inspect {}: {e}", path.display()))?;
            if action.name == "open_path" && !metadata.is_file() && !metadata.is_dir() {
                return Err("Only files and folders can be opened.".into());
            }
            if action.name == "list_directory" && !metadata.is_dir() {
                return Err("Only folders can be listed.".into());
            }
        }
        "read_file" => {
            let path = action_path(&action.arguments, "path", &roots, true)?;
            if !path.is_file() {
                return Err("Only files can be read.".into());
            }
        }
        "create_file" => {
            action_path(&action.arguments, "path", &roots, false)?;
            let content = string_argument(&action.arguments, "content")?;
            if content.len() > MAX_CREATED_BYTES {
                return Err(format!(
                    "New file exceeds the {MAX_CREATED_BYTES}-byte limit."
                ));
            }
        }
        "create_directory" => {
            action_path(&action.arguments, "path", &roots, false)?;
        }
        "move_file" => {
            let source = action_path(&action.arguments, "path", &roots, true)?;
            if !source.is_file() {
                return Err("Only files can be moved.".into());
            }
            action_path(&action.arguments, "destination", &roots, false)?;
        }
        "write_file" | "erase_file_content" => {
            let path = action_path(&action.arguments, "path", &roots, true)?;
            let metadata = fs::metadata(&path)
                .map_err(|e| format!("Cannot inspect {}: {e}", path.display()))?;
            if !metadata.is_file() {
                return Err("Only files can be changed.".into());
            }
            if metadata.len() > MAX_EDIT_BYTES {
                return Err(format!(
                    "File exceeds the {MAX_EDIT_BYTES}-byte change limit."
                ));
            }
            let previous = fs::read(&path)
                .map_err(|e| format!("Cannot read {} before changing it: {e}", path.display()))?;
            if action.name == "write_file" {
                std::str::from_utf8(&previous)
                    .map_err(|_| "Only UTF-8 text files can be replaced.".to_string())?;
                let content = string_argument(&action.arguments, "content")?;
                if content.len() > MAX_CREATED_BYTES {
                    return Err(format!(
                        "New file exceeds the {MAX_CREATED_BYTES}-byte limit."
                    ));
                }
            }
            let preview = String::from_utf8_lossy(&previous);
            action.preview = Some(if preview.chars().count() > MAX_PREVIEW_CHARS {
                format!(
                    "{}… (preview truncated)",
                    preview.chars().take(MAX_PREVIEW_CHARS).collect::<String>()
                )
            } else {
                preview.into_owned()
            });
            action.preview_hash = Some(content_hash(&previous));
        }
        _ => {}
    }
    Ok(action)
}

pub fn execute(action: &AutomationAction, folders: &[String]) -> Result<String, String> {
    if action.name == "open_app" {
        let target = action
            .arguments
            .get("launchTarget")
            .ok_or_else(|| "Choose an exact app match before approving the launch.".to_string())?;
        let kind = target
            .get("kind")
            .and_then(Value::as_str)
            .ok_or_else(|| "The selected app target is invalid.".to_string())?;
        let destination = target
            .get("target")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| "The selected app target is missing.".to_string())?;
        let label = target
            .get("label")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .unwrap_or("application");
        match kind {
            "shortcut" => open_shortcut(Path::new(destination))?,
            "registered" => open_registered_app(destination)?,
            _ => return Err("The selected app target is unsupported.".into()),
        }
        return Ok(format!("Opened {label}."));
    }

    let roots = authorized_roots(folders)?;
    match action.name.as_str() {
        "list_directory" => {
            let path = action_path(&action.arguments, "path", &roots, true)?;
            let mut entries = Vec::new();
            let mut omitted = 0;
            for entry in
                fs::read_dir(&path).map_err(|e| format!("Cannot list {}: {e}", path.display()))?
            {
                let entry = entry.map_err(|e| format!("Cannot read directory entry: {e}"))?;
                if entries.len() == MAX_LIST_ENTRIES {
                    omitted += 1;
                    continue;
                }
                let kind = if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    "folder"
                } else {
                    "file"
                };
                entries.push(format!("{kind}: {}", entry.file_name().to_string_lossy()));
            }
            entries.sort();
            if omitted > 0 {
                entries.push(format!("… and {omitted} more entries"));
            }
            Ok(entries.join("\n"))
        }
        "read_file" => {
            let path = action_path(&action.arguments, "path", &roots, true)?;
            let metadata = fs::metadata(&path)
                .map_err(|e| format!("Cannot inspect {}: {e}", path.display()))?;
            if !metadata.is_file() {
                return Err("Only files can be read.".into());
            }
            if metadata.len() > MAX_READ_BYTES {
                return Err(format!(
                    "File exceeds the {MAX_READ_BYTES}-byte read limit."
                ));
            }
            fs::read_to_string(&path).map_err(|e| format!("Cannot read text file: {e}"))
        }
        "open_path" => {
            let path = action_path(&action.arguments, "path", &roots, true)?;
            open_in_file_manager(&path)?;
            Ok(format!("Opened {} in the file manager.", path.display()))
        }
        "create_directory" => {
            let path = action_path(&action.arguments, "path", &roots, false)?;
            fs::create_dir(&path).map_err(|e| format!("Cannot create {}: {e}", path.display()))?;
            Ok(format!("Created folder {}.", path.display()))
        }
        "create_file" => {
            let path = action_path(&action.arguments, "path", &roots, false)?;
            let content = string_argument(&action.arguments, "content")?;
            if content.len() > MAX_CREATED_BYTES {
                return Err(format!(
                    "New file exceeds the {MAX_CREATED_BYTES}-byte limit."
                ));
            }
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .map_err(|e| {
                    format!("Cannot create {} without overwriting: {e}", path.display())
                })?;
            use std::io::Write;
            if let Err(error) = file.write_all(content.as_bytes()) {
                drop(file);
                let cleanup = fs::remove_file(&path);
                return Err(match cleanup {
                    Ok(()) => format!("Could not write {}: {error}", path.display()),
                    Err(cleanup_error) => format!(
                        "Could not write {}: {error}; partial file cleanup also failed: {cleanup_error}",
                        path.display()
                    ),
                });
            }
            Ok(format!("Created {}.", path.display()))
        }
        "write_file" | "erase_file_content" => {
            let path = action_path(&action.arguments, "path", &roots, true)?;
            let metadata = fs::metadata(&path)
                .map_err(|e| format!("Cannot inspect {}: {e}", path.display()))?;
            if !metadata.is_file() {
                return Err("Only files can be changed.".into());
            }
            if metadata.len() > MAX_EDIT_BYTES {
                return Err(format!(
                    "File exceeds the {MAX_EDIT_BYTES}-byte change limit."
                ));
            }
            let content = if action.name == "write_file" {
                let content = string_argument(&action.arguments, "content")?;
                if content.len() > MAX_CREATED_BYTES {
                    return Err(format!(
                        "New file exceeds the {MAX_CREATED_BYTES}-byte limit."
                    ));
                }
                content.as_bytes()
            } else {
                &[]
            };
            let previous = fs::read(&path)
                .map_err(|e| format!("Cannot read {} before changing it: {e}", path.display()))?;
            if action.name == "write_file" {
                std::str::from_utf8(&previous)
                    .map_err(|_| "Only UTF-8 text files can be replaced.".to_string())?;
            }
            if action.preview_hash != Some(content_hash(&previous)) {
                return Err(
                    "The file changed after the preview. Ask again to review its current contents."
                        .into(),
                );
            }
            let backup = create_backup(&path, &previous)?;
            let result = (|| -> std::io::Result<()> {
                use std::io::Write;
                let mut file = fs::OpenOptions::new()
                    .write(true)
                    .truncate(true)
                    .open(&path)?;
                file.write_all(content)?;
                file.sync_all()
            })();
            if let Err(error) = result {
                return match restore_file(&path, &previous) {
                    Ok(()) => Err(format!("Could not update {}: {error}. Original content was restored; backup: {}", path.display(), backup.display())),
                    Err(restore_error) => Err(format!("Could not update {}: {error}; restoring the original also failed: {restore_error}. Backup preserved at {}", path.display(), backup.display())),
                };
            }
            Ok(format!(
                "{} {}. Original content backed up to {}.",
                if action.name == "write_file" {
                    "Updated"
                } else {
                    "Cleared contents of"
                },
                path.display(),
                backup.display()
            ))
        }
        "move_file" => {
            let source = action_path(&action.arguments, "path", &roots, true)?;
            if !source.is_file() {
                return Err("Only files can be moved.".into());
            }
            let destination = action_path(&action.arguments, "destination", &roots, false)?;
            if destination.exists() {
                return Err("The destination already exists; nothing was overwritten.".into());
            }
            fs::hard_link(&source, &destination)
                .map_err(|e| format!("Cannot move file without overwriting: {e}"))?;
            if let Err(error) = fs::remove_file(&source) {
                let cleanup = fs::remove_file(&destination);
                return Err(match cleanup {
                    Ok(()) => format!("Could not finish moving the file: {error}"),
                    Err(cleanup_error) => format!(
                        "Could not remove the original after linking: {error}; destination cleanup also failed: {cleanup_error}"
                    ),
                });
            }
            Ok(format!(
                "Moved {} to {}.",
                source.display(),
                destination.display()
            ))
        }
        _ => Err("This automation action is not supported.".into()),
    }
}

fn authorized_roots(folders: &[String]) -> Result<Vec<PathBuf>, String> {
    let mut roots = Vec::new();
    for folder in folders {
        let path = fs::canonicalize(folder)
            .map_err(|e| format!("Authorized folder is unavailable: {e}"))?;
        if !path.is_dir() {
            return Err(format!(
                "Authorized path is not a folder: {}",
                path.display()
            ));
        }
        if !roots.contains(&path) {
            roots.push(path);
        }
    }
    if roots.is_empty() {
        return Err("Choose at least one authorized folder in Settings first.".into());
    }
    Ok(roots)
}

fn action_path(
    arguments: &Value,
    key: &str,
    roots: &[PathBuf],
    must_exist: bool,
) -> Result<PathBuf, String> {
    let raw = required_string(arguments, key)?;
    let input = Path::new(raw);
    let components = input.components().collect::<Vec<_>>();
    if components
        .iter()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err("Paths cannot contain '..'.".into());
    }
    if !input.is_absolute()
        && components
            .iter()
            .any(|component| matches!(component, Component::RootDir | Component::Prefix(_)))
    {
        return Err("Relative paths must not include a drive or root.".into());
    }

    let candidates = if input.is_absolute() {
        vec![input.to_path_buf()]
    } else {
        roots
            .iter()
            .map(|root| {
                if input == Path::new(".")
                    || input.file_name().is_some_and(|name| {
                        root.file_name().is_some_and(|root_name| {
                            name.to_string_lossy()
                                .eq_ignore_ascii_case(&root_name.to_string_lossy())
                        })
                    })
                {
                    root.clone()
                } else {
                    root.join(input)
                }
            })
            .collect()
    };
    let mut last_error = None;
    for candidate in candidates {
        let resolved = if must_exist {
            match fs::canonicalize(&candidate) {
                Ok(path) => path,
                Err(error) => {
                    last_error = Some(error);
                    continue;
                }
            }
        } else {
            let parent = candidate
                .parent()
                .ok_or_else(|| "A new item must be inside an authorized folder.".to_string())?;
            let canonical_parent = match fs::canonicalize(parent) {
                Ok(path) => path,
                Err(error) => {
                    last_error = Some(error);
                    continue;
                }
            };
            let name = candidate
                .file_name()
                .ok_or_else(|| "A new item must have a file or folder name.".to_string())?;
            canonical_parent.join(name)
        };
        if roots.iter().any(|root| resolved.starts_with(root)) {
            return Ok(resolved);
        }
    }
    if let Some(error) = last_error {
        return Err(format!(
            "Path is unavailable or outside authorized folders: {error}"
        ));
    }
    Err("That path is outside the folders authorized in Settings.".into())
}

fn required_string<'a>(arguments: &'a Value, key: &str) -> Result<&'a str, String> {
    arguments
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| format!("Missing action argument: {key}."))
}

fn string_argument<'a>(arguments: &'a Value, key: &str) -> Result<&'a str, String> {
    arguments
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("Missing action argument: {key}."))
}
fn content_hash(bytes: &[u8]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    hasher.finish()
}

fn create_backup(path: &Path, content: &[u8]) -> Result<PathBuf, String> {
    use std::io::Write;
    let parent = path
        .parent()
        .ok_or_else(|| "Cannot create a backup for this file.".to_string())?;
    let name = path
        .file_name()
        .ok_or_else(|| "Cannot create a backup for this file.".to_string())?
        .to_string_lossy();
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| format!("System clock error while creating backup: {e}"))?
        .as_secs();
    for suffix in 0..1000u16 {
        let backup = parent.join(format!("{name}.coucou-backup-{timestamp}-{suffix}"));
        let mut file = match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&backup)
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(format!(
                    "Could not create backup {}: {error}",
                    backup.display()
                ))
            }
        };
        if let Err(error) = file.write_all(content).and_then(|()| file.sync_all()) {
            drop(file);
            return match fs::remove_file(&backup) {
                Ok(()) => Err(format!("Could not write backup {}: {error}", backup.display())),
                Err(cleanup_error) => Err(format!(
                    "Could not write backup {}: {error}; partial backup cleanup failed: {cleanup_error}",
                    backup.display()
                )),
            };
        }
        return Ok(backup);
    }
    Err("Could not reserve a unique backup filename.".into())
}

fn restore_file(path: &Path, content: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(path)?;
    file.write_all(content)?;
    file.sync_all()
}

fn normalize_app_name(name: &str) -> String {
    name.chars()
        .filter(|character| character.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn exact_app_name_match(installed_name: &str, requested_name: &str) -> bool {
    let requested = normalize_app_name(requested_name);
    !requested.is_empty() && normalize_app_name(installed_name) == requested
}

#[cfg(target_os = "windows")]
fn find_app_targets(name: &str) -> Result<Vec<AppLaunchTarget>, String> {
    let wanted = normalize_app_name(name);
    if wanted.is_empty() {
        return Ok(Vec::new());
    }
    let targets = start_app_targets(windows_start_apps()?, &wanted);
    if !targets.is_empty() {
        return Ok(targets);
    }

    let mut roots = Vec::new();
    if let Some(appdata) = std::env::var_os("APPDATA") {
        roots.push((
            PathBuf::from(appdata).join("Microsoft\\Windows\\Start Menu\\Programs"),
            "user Start menu",
        ));
    }
    if let Some(program_data) = std::env::var_os("PROGRAMDATA") {
        roots.push((
            PathBuf::from(program_data).join("Microsoft\\Windows\\Start Menu\\Programs"),
            "all users Start menu",
        ));
    }
    let mut shortcuts = Vec::new();
    for (root, origin) in roots {
        let mut matches = Vec::new();
        collect_shortcuts(&root, &wanted, 0, &mut matches);
        shortcuts.extend(matches.into_iter().map(|path| (path, origin)));
    }
    shortcuts.sort_by(|left, right| left.0.cmp(&right.0));
    shortcuts.dedup_by(|left, right| left.0 == right.0);
    let multiple = shortcuts.len() > 1;
    Ok(shortcuts
        .into_iter()
        .map(|(shortcut, origin)| {
            let stem = shortcut
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or(name);
            let label = if multiple {
                let group = shortcut
                    .parent()
                    .and_then(Path::file_name)
                    .and_then(|part| part.to_str())
                    .unwrap_or(origin);
                format!("{stem} in {group} ({origin})")
            } else {
                stem.to_string()
            };
            let target = shortcut.to_string_lossy().into_owned();
            AppLaunchTarget {
                id: format!("shortcut:{target}"),
                label,
                kind: "shortcut".into(),
                target,
            }
        })
        .collect())
}

#[cfg(target_os = "windows")]
#[derive(Deserialize)]
struct WindowsStartApp {
    #[serde(rename = "Name")]
    name: String,
    #[serde(rename = "AppID")]
    app_id: String,
}

#[cfg(target_os = "windows")]
fn start_app_targets(apps: Vec<WindowsStartApp>, wanted: &str) -> Vec<AppLaunchTarget> {
    let mut targets = apps
        .into_iter()
        .filter(|app| exact_app_name_match(&app.name, wanted))
        .map(|app| AppLaunchTarget {
            id: format!("registered:{}", app.app_id),
            label: app.name,
            kind: "registered".into(),
            target: app.app_id,
        })
        .collect::<Vec<_>>();
    targets.sort_by(|left, right| left.id.cmp(&right.id));
    targets.dedup_by(|left, right| left.id == right.id);
    targets
}

#[cfg(target_os = "windows")]
fn windows_start_apps() -> Result<Vec<WindowsStartApp>, String> {
    const POWERSHELL_SCRIPT: &str = concat!(
        "$OutputEncoding = [Console]::OutputEncoding = ",
        "New-Object System.Text.UTF8Encoding $false; ",
        "Get-StartApps | Select-Object Name, AppID | ConvertTo-Json -Compress"
    );
    let output = Command::new("powershell.exe")
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            POWERSHELL_SCRIPT,
        ])
        .output()
        .map_err(|error| format!("Could not query Windows Start apps: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "Windows Start-app discovery failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let output = String::from_utf8(output.stdout)
        .map_err(|error| format!("Windows Start-app list was not valid UTF-8: {error}"))?;
    parse_windows_start_apps(&output)
}

#[cfg(target_os = "windows")]
fn parse_windows_start_apps(output: &str) -> Result<Vec<WindowsStartApp>, String> {
    if output.trim().is_empty() {
        return Ok(Vec::new());
    }
    let value: Value = serde_json::from_str(output)
        .map_err(|error| format!("Could not parse Windows Start-app list: {error}"))?;
    let entries = match value {
        Value::Array(entries) => entries,
        Value::Object(object) => vec![Value::Object(object)],
        _ => return Err("Windows returned an invalid Start-app list.".into()),
    };
    entries
        .into_iter()
        .map(|entry| {
            serde_json::from_value(entry)
                .map_err(|error| format!("Windows returned an invalid Start-app entry: {error}"))
        })
        .collect()
}

#[cfg(target_os = "windows")]
fn collect_shortcuts(directory: &Path, wanted: &str, depth: usize, matches: &mut Vec<PathBuf>) {
    if depth > 6 {
        return;
    }
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    let mut directories = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            directories.push(path);
        } else if kind.is_file()
            && path
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("lnk"))
            && path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .is_some_and(|stem| exact_app_name_match(stem, wanted))
        {
            matches.push(path);
        }
    }
    directories.sort();
    for path in directories {
        collect_shortcuts(&path, wanted, depth + 1, matches);
    }
}

#[cfg(target_os = "linux")]
fn find_app_targets(name: &str) -> Result<Vec<AppLaunchTarget>, String> {
    let wanted = normalize_app_name(name);
    let mut roots = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        roots.push(PathBuf::from(home).join(".local/share/applications"));
    }
    roots.push(PathBuf::from("/usr/share/applications"));
    let mut targets = Vec::new();
    for root in roots {
        collect_desktop_files(&root, &wanted, &mut targets);
    }
    targets.sort_by(|left, right| left.id.cmp(&right.id));
    targets.dedup_by(|left, right| left.id == right.id);
    if targets.len() > 1 {
        for target in &mut targets {
            let path = Path::new(&target.target);
            let group = path
                .parent()
                .and_then(Path::file_name)
                .and_then(|part| part.to_str())
                .unwrap_or("applications");
            target.label = format!("{} in {group}", target.label);
        }
    }
    Ok(targets)
}

#[cfg(target_os = "linux")]
fn collect_desktop_files(directory: &Path, wanted: &str, targets: &mut Vec<AppLaunchTarget>) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("desktop"))
        {
            continue;
        }
        let stem_matches = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .is_some_and(|stem| exact_app_name_match(stem, wanted));
        let display_name_matches = fs::read_to_string(&path).ok().is_some_and(|contents| {
            contents.lines().any(|line| {
                line.strip_prefix("Name=")
                    .is_some_and(|installed_name| exact_app_name_match(installed_name, wanted))
            })
        });
        if stem_matches || display_name_matches {
            let label = fs::read_to_string(&path)
                .ok()
                .and_then(|contents| {
                    contents.lines().find_map(|line| {
                        line.strip_prefix("Name=")
                            .filter(|installed_name| exact_app_name_match(installed_name, wanted))
                            .map(str::to_string)
                    })
                })
                .or_else(|| {
                    path.file_stem()
                        .and_then(|stem| stem.to_str())
                        .map(str::to_string)
                })
                .unwrap_or_else(|| wanted.clone());
            let target = path.to_string_lossy().into_owned();
            targets.push(AppLaunchTarget {
                id: format!("desktop:{target}"),
                label,
                kind: "shortcut".into(),
                target,
            });
        }
    }
}

#[cfg(not(any(target_os = "windows", target_os = "linux")))]
fn find_app_targets(_name: &str) -> Result<Vec<AppLaunchTarget>, String> {
    Err("Opening apps with local automation is not supported on this platform.".into())
}

#[cfg(target_os = "windows")]
fn open_in_file_manager(path: &Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    let wide = path
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let result = unsafe {
        ShellExecuteW(
            None,
            None,
            windows::core::PCWSTR(wide.as_ptr()),
            None,
            None,
            SW_SHOWNORMAL,
        )
    };
    if result.0 as isize <= 32 {
        return Err(format!(
            "Windows could not open {} (error {}).",
            path.display(),
            result.0 as isize
        ));
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn open_shortcut(shortcut: &Path) -> Result<(), String> {
    open_in_file_manager(shortcut)
}

#[cfg(target_os = "windows")]
fn open_registered_app(app_id: &str) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    let target = format!("shell:AppsFolder\\{app_id}");
    let wide = std::ffi::OsStr::new(&target)
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let result = unsafe {
        ShellExecuteW(
            None,
            None,
            windows::core::PCWSTR(wide.as_ptr()),
            None,
            None,
            SW_SHOWNORMAL,
        )
    };
    if result.0 as isize <= 32 {
        return Err(format!(
            "Windows could not open the registered app (error {}).",
            result.0 as isize
        ));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn open_in_file_manager(path: &Path) -> Result<(), String> {
    Command::new("xdg-open")
        .arg(path)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("Could not open the item: {e}"))
}

#[cfg(target_os = "linux")]
fn open_shortcut(shortcut: &Path) -> Result<(), String> {
    let app_id = shortcut
        .file_stem()
        .ok_or_else(|| "The desktop entry has no app ID.".to_string())?;
    Command::new("gtk-launch")
        .arg(app_id)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("Could not launch the app: {e}"))
}

#[cfg(target_os = "linux")]
fn open_registered_app(_app_id: &str) -> Result<(), String> {
    Err("This registered app launch method is only available on Windows.".into())
}

#[cfg(not(any(target_os = "windows", target_os = "linux")))]
fn open_in_file_manager(_path: &Path) -> Result<(), String> {
    Err("Opening items from automation is not supported on this platform.".into())
}

#[cfg(not(any(target_os = "windows", target_os = "linux")))]
fn open_shortcut(_shortcut: &Path) -> Result<(), String> {
    Err("Opening apps is not supported on this platform.".into())
}

#[cfg(not(any(target_os = "windows", target_os = "linux")))]
fn open_registered_app(_app_id: &str) -> Result<(), String> {
    Err("Opening apps is not supported on this platform.".into())
}
#[cfg(test)]
mod tests {
    use super::{execute, AutomationAction};
    use serde_json::json;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn test_root() -> std::path::PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "coucou-automation-{}-{}",
            std::process::id(),
            unique
        ));
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn action(name: &str, arguments: serde_json::Value) -> AutomationAction {
        AutomationAction {
            name: name.into(),
            arguments,
            preview: None,
            preview_hash: None,
        }
    }

    #[test]
    fn explicit_open_requests_are_routed_as_local_actions() {
        assert_eq!(
            super::explicit_open_app("open WhatsApp"),
            Some("WhatsApp".into())
        );
        assert_eq!(
            super::explicit_open_app("please open the WhatsApp app"),
            Some("WhatsApp".into())
        );
        assert_eq!(
            super::explicit_open_app("open WhatsApp app please"),
            Some("WhatsApp".into())
        );
        assert_eq!(
            super::explicit_open_path(r"open C:\Users\me\Documents"),
            Some(r"C:\Users\me\Documents".into())
        );
        assert_eq!(
            super::explicit_open_path("open project folder"),
            Some("project".into())
        );
        assert_eq!(super::explicit_open_app("open project folder"), None);
        assert_eq!(super::explicit_open_app("open notes.txt"), None);
        assert_eq!(
            super::explicit_open_path("open notes.txt"),
            Some("notes.txt".into())
        );
        assert_eq!(super::explicit_open_app("tell me about WhatsApp"), None);
    }

    #[test]
    fn app_selection_requires_an_exact_pending_candidate() {
        let mut pending = action(
            "open_app",
            json!({
                "appName": "Editor",
                "appChoices": [
                    {"id": "shortcut:one", "label": "Editor in Personal", "kind": "shortcut", "target": "one"},
                    {"id": "shortcut:two", "label": "Editor in Work", "kind": "shortcut", "target": "two"}
                ]
            }),
        );
        assert!(super::select_app_target(&mut pending, None).is_err());
        assert!(super::select_app_target(&mut pending, Some("shortcut:other")).is_err());
        super::select_app_target(&mut pending, Some("shortcut:two")).unwrap();
        assert_eq!(pending.arguments["launchTarget"]["target"], "two");
        assert!(pending.arguments.get("appChoices").is_none());
    }

    #[test]
    fn app_lookup_requires_the_full_installed_name() {
        assert!(super::exact_app_name_match(
            "Microsoft Teams",
            "Microsoft Teams"
        ));
        assert!(super::exact_app_name_match(
            "Google-Chrome",
            "google chrome"
        ));
        assert!(!super::exact_app_name_match("Microsoft Teams", "Teams"));
        assert!(!super::exact_app_name_match(
            "Visual Studio Code",
            "Visual Studio"
        ));
    }

    #[test]
    fn opening_an_app_without_a_prepared_exact_target_is_refused() {
        let result = execute(&action("open_app", json!({"appName": "Editor"})), &[]);
        assert!(result.unwrap_err().contains("exact app match"));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_start_app_catalog_parses_single_and_multiple_entries() {
        let single = super::parse_windows_start_apps(
            r#"{"Name":"Spotify","AppID":"SpotifyAB.SpotifyMusic_zpdnekdrzrea0!Spotify"}"#,
        )
        .unwrap();
        assert_eq!(single.len(), 1);
        assert_eq!(single[0].name, "Spotify");
        assert_eq!(
            single[0].app_id,
            "SpotifyAB.SpotifyMusic_zpdnekdrzrea0!Spotify"
        );

        let multiple = super::parse_windows_start_apps(
            r#"[{"Name":"Spotify","AppID":"spotify"},{"Name":"Spotify","AppID":"spotify-beta"}]"#,
        )
        .unwrap();
        assert_eq!(multiple.len(), 2);
        assert_eq!(
            super::start_app_targets(multiple, "Spotify")
                .iter()
                .map(|target| target.target.as_str())
                .collect::<Vec<_>>(),
            ["spotify", "spotify-beta"]
        );
        let partial = super::parse_windows_start_apps(
            r#"[{"Name":"Spotify","AppID":"spotify"},{"Name":"Spotify","AppID":"spotify-beta"}]"#,
        )
        .unwrap();
        assert!(super::start_app_targets(partial, "Spot").is_empty());
    }

    #[test]
    fn create_file_refuses_overwrite_and_paths_outside_roots() {
        let root = test_root();
        let folder = root.join("allowed");
        fs::create_dir(&folder).unwrap();
        let outside = root.join("outside.txt");
        fs::write(&outside, "secret").unwrap();
        let folders = vec![folder.to_string_lossy().into_owned()];

        let create = action(
            "create_file",
            json!({"path": "note.txt", "content": "hello"}),
        );
        assert!(execute(&create, &folders).is_ok());
        assert!(execute(&create, &folders).is_err());

        let outside_read = action("read_file", json!({"path": outside.to_string_lossy()}));
        assert!(execute(&outside_read, &folders).is_err());

        let traversal = action("create_directory", json!({"path": "../outside"}));
        assert!(execute(&traversal, &folders).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn files_can_be_listed_read_and_moved_only_inside_authorized_roots() {
        let root = test_root();
        let folder = root.join("allowed");
        fs::create_dir(&folder).unwrap();
        let second_folder = root.join("also-allowed");
        fs::create_dir(&second_folder).unwrap();
        fs::write(second_folder.join("other.txt"), "other").unwrap();
        fs::write(folder.join("note.txt"), "hello").unwrap();
        let folders = vec![
            folder.to_string_lossy().into_owned(),
            second_folder.to_string_lossy().into_owned(),
        ];

        let roots = super::authorized_roots(&folders).unwrap();
        let selected_root =
            super::action_path(&json!({"path": "allowed"}), "path", &roots, true).unwrap();
        assert_eq!(selected_root, fs::canonicalize(&folder).unwrap());

        let listed = execute(&action("list_directory", json!({"path": "."})), &folders).unwrap();
        assert!(listed.contains("note.txt"));
        let read = execute(&action("read_file", json!({"path": "note.txt"})), &folders).unwrap();
        assert_eq!(read, "hello");
        let second_read =
            execute(&action("read_file", json!({"path": "other.txt"})), &folders).unwrap();
        assert_eq!(second_read, "other");
        fs::write(folder.join("occupied.txt"), "keep").unwrap();
        assert!(execute(
            &action(
                "move_file",
                json!({"path": "note.txt", "destination": "occupied.txt"}),
            ),
            &folders,
        )
        .is_err());
        assert_eq!(
            fs::read_to_string(folder.join("occupied.txt")).unwrap(),
            "keep"
        );
        assert!(folder.join("note.txt").exists());
        execute(
            &action(
                "move_file",
                json!({"path": "note.txt", "destination": "moved.txt"}),
            ),
            &folders,
        )
        .unwrap();
        assert!(folder.join("moved.txt").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn write_and_erase_require_preview_and_create_restorable_backups() {
        let root = test_root();
        let folder = root.join("allowed");
        fs::create_dir(&folder).unwrap();
        let file = folder.join("notes.txt");
        fs::write(&file, "original").unwrap();
        let folders = vec![folder.to_string_lossy().into_owned()];

        let write = super::prepare(
            action(
                "write_file",
                json!({"path": "notes.txt", "content": "replacement"}),
            ),
            &folders,
        )
        .unwrap();
        assert_eq!(write.preview.as_deref(), Some("original"));
        let result = super::execute(&write, &folders).unwrap();
        assert!(
            result.contains("notes.txt. Original content backed up to "),
            "{result}"
        );
        assert!(result.contains(".coucou-backup-"), "{result}");
        assert_eq!(fs::read_to_string(&file).unwrap(), "replacement");
        let backup = fs::read_dir(&folder)
            .unwrap()
            .flatten()
            .map(|entry| entry.path())
            .find(|path| path.to_string_lossy().contains(".coucou-backup-"))
            .unwrap();
        assert_eq!(fs::read_to_string(backup).unwrap(), "original");

        let erase = super::prepare(
            action("erase_file_content", json!({"path": "notes.txt"})),
            &folders,
        )
        .unwrap();
        assert_eq!(erase.preview.as_deref(), Some("replacement"));
        super::execute(&erase, &folders).unwrap();
        assert_eq!(fs::read(&file).unwrap(), b"");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn edits_refuse_to_apply_when_file_changed_after_consent_preview() {
        let root = test_root();
        let folder = root.join("allowed");
        fs::create_dir(&folder).unwrap();
        let file = folder.join("notes.txt");
        fs::write(&file, "previewed content").unwrap();
        let folders = vec![folder.to_string_lossy().into_owned()];
        let action = super::prepare(
            action(
                "write_file",
                json!({"path": "notes.txt", "content": "new content"}),
            ),
            &folders,
        )
        .unwrap();
        fs::write(&file, "changed after preview").unwrap();
        let error = super::execute(&action, &folders).unwrap_err();
        assert!(error.contains("file changed after the preview"));
        assert_eq!(fs::read_to_string(file).unwrap(), "changed after preview");
        fs::remove_dir_all(root).unwrap();
    }
}
