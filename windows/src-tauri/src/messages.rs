use std::collections::{HashSet, VecDeque};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter};

use crate::integrations;
use crate::island::WINDOW_LABEL;
use crate::log;

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct MessageNotification {
    id: String,
    app_name: String,
    title: String,
    body: String,
}

#[derive(Default)]
struct SeenNotifications {
    initialized: bool,
    ids: HashSet<u32>,
    order: VecDeque<u32>,
}

static SEEN: OnceLock<Mutex<SeenNotifications>> = OnceLock::new();
static SOURCES: OnceLock<Mutex<std::collections::HashMap<u32, String>>> = OnceLock::new();

#[tauri::command]
pub async fn request_message_access(app: AppHandle) -> Result<String, String> {
    #[cfg(windows)]
    {
        let result = tokio::task::spawn_blocking(|| with_listener(|listener| {
            let status = listener
                .RequestAccessAsync()
                .map_err(win_error)?
                .get()
                .map_err(win_error)?;
            match status {
                windows::UI::Notifications::Management::UserNotificationListenerAccessStatus::Allowed => {
                    Ok("allowed".to_string())
                }
                windows::UI::Notifications::Management::UserNotificationListenerAccessStatus::Denied => {
                    Err("Windows notification access was denied. Enable it in Windows Settings.".into())
                }
                _ => Err("Windows did not grant notification access.".into()),
            }
        }))
        .await
        .map_err(|error| format!("Notification access task failed: {error}"))??;
        poll(app).await?;
        Ok(result)
    }
    #[cfg(not(windows))]
    {
        let _ = app;
        Err("Windows notification access is available only in the packaged Windows app.".into())
    }
}

#[tauri::command]
pub async fn message_access_status() -> Result<bool, String> {
    #[cfg(windows)]
    {
        return tokio::task::spawn_blocking(|| with_listener(|listener| {
            Ok(listener.GetAccessStatus().map_err(win_error)?
                == windows::UI::Notifications::Management::UserNotificationListenerAccessStatus::Allowed)
        }))
        .await
        .map_err(|error| format!("Notification status task failed: {error}"))?;
    }
    #[cfg(not(windows))]
    Err("Windows notification access is available only on Windows.".into())
}

#[tauri::command]
pub fn open_message_source(notification_id: String) -> Result<(), String> {
    let id = notification_id
        .parse::<u32>()
        .map_err(|_| "Invalid notification identifier.".to_string())?;
    let sources = SOURCES.get_or_init(|| Mutex::new(std::collections::HashMap::new()));
    let app_id = sources
        .lock()
        .map_err(|_| "Notification source list is unavailable.".to_string())?
        .get(&id)
        .cloned()
        .ok_or_else(|| {
            "The source app for this notification is no longer available.".to_string()
        })?;
    #[cfg(windows)]
    {
        let target = format!("shell:AppsFolder\\{app_id}");
        std::process::Command::new("explorer.exe")
            .arg(target)
            .spawn()
            .map_err(|error| format!("Could not open the notification's app: {error}"))?;
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = app_id;
        Err("Opening notification sources is available only on Windows.".into())
    }
}

pub fn start(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let mut retry_delay = Duration::from_secs(1);
        loop {
            if integrations::PAUSED.load(std::sync::atomic::Ordering::Relaxed)
                || !integrations::enabled(&app, "integration_messages")
            {
                tokio::time::sleep(Duration::from_secs(1)).await;
                continue;
            }
            match poll(app.clone()).await {
                Ok(()) => retry_delay = Duration::from_secs(1),
                Err(error) => {
                    log::line(format!("Windows notification poll failed: {error}"));
                    let _ = app.emit_to(WINDOW_LABEL, "message-access-error", &error);
                    tokio::time::sleep(retry_delay).await;
                    retry_delay = (retry_delay * 2).min(Duration::from_secs(60));
                    continue;
                }
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    });
}

async fn poll(app: AppHandle) -> Result<(), String> {
    #[cfg(windows)]
    {
        let notifications = tokio::task::spawn_blocking(load_notifications)
            .await
            .map_err(|error| format!("Notification polling task failed: {error}"))??;
        process_notifications(&app, notifications);
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = app;
        Ok(())
    }
}

#[cfg(windows)]
struct StoredNotification {
    id: u32,
    app_id: String,
    app_name: String,
    title: String,
    body: String,
}

#[cfg(windows)]
fn process_notifications(app: &AppHandle, notifications: Vec<StoredNotification>) {
    let seen = SEEN.get_or_init(|| Mutex::new(SeenNotifications::default()));
    let Ok(mut seen) = seen.lock() else {
        log::line("Windows notification history lock is unavailable");
        return;
    };
    let ids: HashSet<u32> = notifications.iter().map(|item| item.id).collect();
    if !seen.initialized {
        seen.ids = ids;
        seen.order = seen.ids.iter().copied().collect();
        seen.initialized = true;
        return;
    }
    for notification in notifications {
        if !seen.ids.insert(notification.id) {
            continue;
        }
        seen.order.push_back(notification.id);
        if seen.order.len() > 4096 {
            seen.order.clear();
            seen.ids = ids.clone();
            seen.order.extend(ids.iter().copied());
            if let Some(sources) = SOURCES.get() {
                if let Ok(mut sources) = sources.lock() {
                    sources.retain(|id, _| ids.contains(id));
                }
            }
        }
        let source_map = SOURCES
            .get_or_init(|| Mutex::new(std::collections::HashMap::new()))
            .lock();
        if let Ok(mut source_map) = source_map {
            source_map.insert(notification.id, notification.app_id);
        } else {
            log::line("Windows notification source map lock is unavailable");
            continue;
        }
        let _ = app.emit_to(
            WINDOW_LABEL,
            "message-notification",
            MessageNotification {
                id: notification.id.to_string(),
                app_name: notification.app_name,
                title: notification.title,
                body: notification.body,
            },
        );
    }
    while seen.order.len() > 4096 {
        if let Some(oldest) = seen.order.pop_front() {
            seen.ids.remove(&oldest);
        }
    }
}

#[cfg(windows)]
fn load_notifications() -> Result<Vec<StoredNotification>, String> {
    use windows::UI::Notifications::NotificationKinds;

    with_listener(|listener| {
        let status = listener.GetAccessStatus().map_err(win_error)?;
        match status {
            windows::UI::Notifications::Management::UserNotificationListenerAccessStatus::Allowed => {}
            windows::UI::Notifications::Management::UserNotificationListenerAccessStatus::Denied => {
                return Err("Windows notification access was denied. Enable it in Windows Settings.".into());
            }
            _ => return Err("Windows notification access has not been granted.".into()),
        }
        let items = listener
            .GetNotificationsAsync(NotificationKinds::Toast)
            .map_err(win_error)?
            .get()
            .map_err(win_error)?;
        let count = items.Size().map_err(win_error)?;
        let mut notifications = Vec::with_capacity(count.min(256) as usize);
        for index in 0..count {
            let item = items.GetAt(index).map_err(win_error)?;
            let id = item.Id().map_err(win_error)?;
            let app = item.AppInfo().map_err(win_error)?;
            let display = app.DisplayInfo().map_err(win_error)?;
            let app_name = display.DisplayName().map_err(win_error)?.to_string();
            let app_id = app.AppUserModelId().map_err(win_error)?.to_string();
            let Ok(binding) = item
                .Notification()
                .and_then(|notification| notification.Visual())
                .and_then(|visual| {
                    visual.GetBinding(&windows::core::HSTRING::from("ToastGeneric"))
                })
            else {
                continue;
            };
            let text_nodes = binding.GetTextElements().map_err(win_error)?;
            let text_count = text_nodes.Size().map_err(win_error)?;
            let texts = (0..text_count)
                .filter_map(|i| text_nodes.GetAt(i).ok())
                .filter_map(|node| node.Text().ok())
                .map(|value| value.to_string())
                .filter(|value| !value.trim().is_empty())
                .collect::<Vec<_>>();
            if texts.is_empty() || app_name.trim().is_empty() || app_id.trim().is_empty() {
                continue;
            }
            notifications.push(StoredNotification {
                id,
                app_id,
                app_name,
                title: texts[0].clone(),
                body: texts
                    .iter()
                    .skip(1)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(" · "),
            });
        }
        Ok(notifications)
    })
}

#[cfg(windows)]
fn with_listener<T>(
    callback: impl FnOnce(
        &windows::UI::Notifications::Management::UserNotificationListener,
    ) -> Result<T, String>,
) -> Result<T, String> {
    use windows::Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED};
    use windows::UI::Notifications::Management::UserNotificationListener;

    unsafe { RoInitialize(RO_INIT_MULTITHREADED) }.map_err(win_error)?;
    let result = UserNotificationListener::Current()
        .map_err(|error| {
            format!(
                "The Windows notification listener is unavailable. Install the signed MSIX build and grant notification access. {}",
                win_error(error)
            )
        })
        .and_then(|listener| callback(&listener));
    unsafe { windows::Win32::System::WinRT::RoUninitialize() };
    result
}

#[cfg(windows)]
fn win_error(error: windows::core::Error) -> String {
    format!("{} (0x{:08X})", error.message(), error.code().0 as u32)
}
