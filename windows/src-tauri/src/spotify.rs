use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::Ordering;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use rand::RngCore;
use reqwest::{Client, Method, Response, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::Digest;
use tauri::{AppHandle, Emitter};
use url::Url;

use crate::integrations;
use crate::island::WINDOW_LABEL;
use crate::{log, secrets};

const CLIENT_ID_KEY: &str = "spotify-client-id";
const TOKEN_KEY: &str = "spotify-oauth-token";
const AUTHORIZE_URL: &str = "https://accounts.spotify.com/authorize";
const TOKEN_URL: &str = "https://accounts.spotify.com/api/token";
const API_ROOT: &str = "https://api.spotify.com/v1";
const REDIRECT_URI: &str = "http://127.0.0.1:43821/callback";
const POLL_INTERVAL: Duration = Duration::from_secs(4);
const REDIRECT_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
struct OAuthToken {
    access_token: String,
    refresh_token: String,
    expires_at: u64,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: u64,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct SpotifyTrack {
    name: String,
    artists: String,
    image_url: Option<String>,
    duration_ms: u64,
    uri: Option<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct SpotifySnapshot {
    connected: bool,
    playing: bool,
    progress_ms: u64,
    track: Option<SpotifyTrack>,
    queue: Vec<SpotifyTrack>,
}

#[tauri::command]
pub fn spotify_connect(app: AppHandle) -> Result<String, String> {
    let client_id = secrets::get(CLIENT_ID_KEY)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "Save your Spotify Client ID in Settings first.".to_string())?;
    let listener = TcpListener::bind(("127.0.0.1", 43821)).map_err(|error| {
        format!("Could not reserve Spotify sign-in callback port 43821: {error}")
    })?;
    listener
        .set_nonblocking(true)
        .map_err(|error| format!("Could not configure Spotify callback: {error}"))?;
    let redirect_uri = REDIRECT_URI.to_string();
    let state = random_url_token(24);
    let verifier = random_url_token(64);
    let challenge = URL_SAFE_NO_PAD.encode(sha2::Sha256::digest(verifier.as_bytes()));
    let mut auth_url = Url::parse(AUTHORIZE_URL).map_err(|error| error.to_string())?;
    auth_url
        .query_pairs_mut()
        .append_pair("client_id", &client_id)
        .append_pair("response_type", "code")
        .append_pair("redirect_uri", &redirect_uri)
        .append_pair("state", &state)
        .append_pair("code_challenge_method", "S256")
        .append_pair("code_challenge", &challenge)
        .append_pair(
            "scope",
            "user-read-currently-playing user-read-playback-state user-modify-playback-state",
        );

    std::thread::Builder::new()
        .name("coucou-spotify-oauth".into())
        .spawn(move || {
            if let Err(error) =
                receive_oauth_code(listener, &client_id, &redirect_uri, &state, &verifier)
            {
                log::line(format!("Spotify sign-in failed: {error}"));
                let _ = app.emit_to(WINDOW_LABEL, "spotify-error", error);
            }
        })
        .map_err(|error| format!("Could not start Spotify sign-in: {error}"))?;
    Ok(auth_url.to_string())
}

#[tauri::command]
pub async fn spotify_control(
    action: String,
    position_ms: Option<u64>,
    track_uri: Option<String>,
) -> Result<(), String> {
    let mut token = valid_token().await?;
    let (method, endpoint, body) = match action.as_str() {
        "play" => (Method::PUT, format!("{API_ROOT}/me/player/play"), None),
        "play_track" => {
            let uri = track_uri
                .filter(|uri| uri.starts_with("spotify:track:"))
                .ok_or_else(|| "A valid Spotify track URI is required.".to_string())?;
            (
                Method::PUT,
                format!("{API_ROOT}/me/player/play"),
                Some(serde_json::json!({ "uris": [uri] })),
            )
        }
        "pause" => (Method::PUT, format!("{API_ROOT}/me/player/pause"), None),
        "next" => (Method::POST, format!("{API_ROOT}/me/player/next"), None),
        "previous" => (Method::POST, format!("{API_ROOT}/me/player/previous"), None),
        "seek" => {
            let position = position_ms.ok_or_else(|| "Track position is required.".to_string())?;
            if position > i32::MAX as u64 {
                return Err("Track position is out of range.".into());
            }
            (
                Method::PUT,
                format!("{API_ROOT}/me/player/seek?position_ms={position}"),
                None,
            )
        }
        _ => return Err("Unsupported Spotify playback action.".into()),
    };
    let device_id =
        playback_device_id(&mut token, action == "play" || action == "play_track").await?;
    let mut endpoint = Url::parse(&endpoint)
        .map_err(|error| format!("Spotify playback URL was invalid: {error}"))?;
    endpoint
        .query_pairs_mut()
        .append_pair("device_id", &device_id);
    let response = match body {
        Some(body) => {
            authorized_json_request(&method, endpoint.as_str(), &mut token, &body).await?
        }
        None => authorized_request(&method, endpoint.as_str(), &mut token).await?,
    };
    if response.status().is_success() || response.status() == StatusCode::NO_CONTENT {
        return Ok(());
    }
    Err(api_error(response).await)
}

async fn playback_device_id(
    token: &mut OAuthToken,
    allow_inactive: bool,
) -> Result<String, String> {
    let response = authorized_request(
        &Method::GET,
        &format!("{API_ROOT}/me/player/devices"),
        token,
    )
    .await?;
    if !response.status().is_success() {
        return Err(api_error(response).await);
    }

    let devices: Value = response
        .json()
        .await
        .map_err(|error| format!("Spotify devices response was invalid: {error}"))?;
    choose_playback_device(&devices, allow_inactive).ok_or_else(|| {
        if allow_inactive {
            "No available Spotify device. Open Spotify on a device and try again.".to_string()
        } else {
            "No active Spotify device. Start Spotify playback and try again.".to_string()
        }
    })
}

fn choose_playback_device(devices: &Value, allow_inactive: bool) -> Option<String> {
    let devices = devices.get("devices")?.as_array()?;
    let active = devices.iter().find(|device| {
        usable_spotify_device(device)
            && device.get("is_active").and_then(Value::as_bool) == Some(true)
    });
    let selected = active.or_else(|| {
        allow_inactive
            .then(|| devices.iter().find(|device| usable_spotify_device(device)))
            .flatten()
    });
    selected
        .and_then(|device| device.get("id").and_then(Value::as_str))
        .map(ToOwned::to_owned)
}

fn usable_spotify_device(device: &Value) -> bool {
    device.get("is_restricted").and_then(Value::as_bool) != Some(true)
        && device
            .get("id")
            .and_then(Value::as_str)
            .is_some_and(|id| !id.is_empty())
}

#[tauri::command]
pub fn spotify_disconnect(app: AppHandle) -> Result<(), String> {
    secrets::clear(TOKEN_KEY)?;
    app.emit_to(
        WINDOW_LABEL,
        "spotify-update",
        SpotifySnapshot {
            connected: false,
            playing: false,
            progress_ms: 0,
            track: None,
            queue: Vec::new(),
        },
    )
    .map_err(|error| format!("Could not update Spotify connection status: {error}"))
}

pub fn start(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let mut retry_delay = POLL_INTERVAL;
        loop {
            if integrations::PAUSED.load(Ordering::Relaxed)
                || !integrations::enabled(&app, "integration_spotify")
            {
                tokio::time::sleep(POLL_INTERVAL).await;
                continue;
            }
            if secrets::get(CLIENT_ID_KEY).is_none() || secrets::get(TOKEN_KEY).is_none() {
                tokio::time::sleep(POLL_INTERVAL).await;
                continue;
            }
            match publish_snapshot(&app).await {
                Ok(()) => retry_delay = POLL_INTERVAL,
                Err(error) => {
                    log::line(format!("Spotify playback refresh failed: {error}"));
                    let _ = app.emit_to(WINDOW_LABEL, "spotify-error", error);
                    tokio::time::sleep(retry_delay).await;
                    retry_delay = (retry_delay * 2).min(Duration::from_secs(60));
                    continue;
                }
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    });
}

pub async fn poll_once(app: AppHandle) {
    if let Err(error) = publish_snapshot(&app).await {
        log::line(format!("Spotify refresh failed: {error}"));
        let _ = app.emit_to(WINDOW_LABEL, "spotify-error", error);
    }
}

async fn publish_snapshot(app: &AppHandle) -> Result<(), String> {
    let mut token = valid_token().await?;
    let current = authorized_request(
        &Method::GET,
        &format!("{API_ROOT}/me/player/currently-playing"),
        &mut token,
    )
    .await?;
    let (playing, progress_ms, track) = if current.status() == StatusCode::NO_CONTENT {
        (false, 0, None)
    } else if current.status().is_success() {
        let value: Value = current
            .json()
            .await
            .map_err(|error| format!("Spotify playback response was invalid: {error}"))?;
        let playing = value
            .get("is_playing")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let progress_ms = value
            .get("progress_ms")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let track = value.get("item").and_then(track_from_value);
        (playing, progress_ms, track)
    } else {
        return Err(api_error(current).await);
    };

    let queue_response = authorized_request(
        &Method::GET,
        &format!("{API_ROOT}/me/player/queue"),
        &mut token,
    )
    .await?;
    let queue = if queue_response.status().is_success() {
        let value: Value = queue_response
            .json()
            .await
            .map_err(|error| format!("Spotify queue response was invalid: {error}"))?;
        value
            .get("queue")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(track_from_value)
            .take(20)
            .collect()
    } else if queue_response.status() == StatusCode::NO_CONTENT {
        Vec::new()
    } else {
        return Err(api_error(queue_response).await);
    };

    let snapshot = SpotifySnapshot {
        connected: true,
        playing,
        progress_ms,
        track,
        queue,
    };
    app.emit_to(WINDOW_LABEL, "spotify-update", snapshot)
        .map_err(|error| format!("Could not update Spotify player: {error}"))
}

fn track_from_value(value: &Value) -> Option<SpotifyTrack> {
    let name = value.get("name")?.as_str()?.to_string();
    let artists = value
        .get("artists")?
        .as_array()?
        .iter()
        .filter_map(|artist| artist.get("name").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join(", ");
    let duration_ms = value.get("duration_ms")?.as_u64()?;
    let image_url = value
        .get("album")
        .and_then(|album| album.get("images"))
        .and_then(Value::as_array)
        .and_then(|images| images.first())
        .and_then(|image| image.get("url"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let uri = value
        .get("uri")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    Some(SpotifyTrack {
        name,
        artists,
        image_url,
        duration_ms,
        uri,
    })
}

async fn authorized_request(
    method: &Method,
    endpoint: &str,
    token: &mut OAuthToken,
) -> Result<Response, String> {
    authorized_request_with_body(method, endpoint, token, None).await
}

async fn authorized_json_request(
    method: &Method,
    endpoint: &str,
    token: &mut OAuthToken,
    body: &Value,
) -> Result<Response, String> {
    authorized_request_with_body(method, endpoint, token, Some(body)).await
}

async fn authorized_request_with_body(
    method: &Method,
    endpoint: &str,
    token: &mut OAuthToken,
    body: Option<&Value>,
) -> Result<Response, String> {
    let client = http_client()?;
    let mut request = client
        .request(method.clone(), endpoint)
        .bearer_auth(&token.access_token);
    if let Some(body) = body {
        request = request.json(body);
    }
    let response = request
        .send()
        .await
        .map_err(|error| format!("Spotify request failed: {error}"))?;
    if response.status() != StatusCode::UNAUTHORIZED {
        return Ok(response);
    }
    *token = refresh_token(&token.refresh_token).await?;
    let mut retry = client
        .request(method.clone(), endpoint)
        .bearer_auth(&token.access_token);
    if let Some(body) = body {
        retry = retry.json(body);
    }
    retry
        .send()
        .await
        .map_err(|error| format!("Spotify retry failed: {error}"))
}

async fn valid_token() -> Result<OAuthToken, String> {
    let mut token: OAuthToken = secrets::get(TOKEN_KEY)
        .ok_or_else(|| "Connect Spotify from Settings to sign in.".to_string())
        .and_then(|value| {
            serde_json::from_str(&value)
                .map_err(|error| format!("Stored Spotify sign-in is invalid: {error}"))
        })?;
    let now = unix_time()?;
    if token.expires_at <= now + 60 {
        token = refresh_token(&token.refresh_token).await?;
    }
    Ok(token)
}

async fn refresh_token(refresh: &str) -> Result<OAuthToken, String> {
    let client_id = secrets::get(CLIENT_ID_KEY)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "Save your Spotify Client ID in Settings first.".to_string())?;
    let client = http_client()?;
    let response = client
        .post(TOKEN_URL)
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh),
            ("client_id", client_id.as_str()),
        ])
        .send()
        .await
        .map_err(|error| format!("Spotify token refresh failed: {error}"))?;
    if !response.status().is_success() {
        return Err(api_error(response).await);
    }
    let result: TokenResponse = response
        .json()
        .await
        .map_err(|error| format!("Spotify token response was invalid: {error}"))?;
    let token = OAuthToken {
        access_token: result.access_token,
        refresh_token: result.refresh_token.unwrap_or_else(|| refresh.to_string()),
        expires_at: unix_time()?.saturating_add(result.expires_in),
    };
    store_token(&token)?;
    Ok(token)
}

async fn exchange_code(
    client_id: &str,
    redirect_uri: &str,
    code: &str,
    verifier: &str,
) -> Result<(), String> {
    let response = http_client()?
        .post(TOKEN_URL)
        .form(&[
            ("client_id", client_id),
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect_uri),
            ("code_verifier", verifier),
        ])
        .send()
        .await
        .map_err(|error| format!("Spotify sign-in exchange failed: {error}"))?;
    if !response.status().is_success() {
        return Err(api_error(response).await);
    }
    let result: TokenResponse = response
        .json()
        .await
        .map_err(|error| format!("Spotify token response was invalid: {error}"))?;
    let refresh_token = result
        .refresh_token
        .ok_or_else(|| "Spotify did not return a refresh token.".to_string())?;
    store_token(&OAuthToken {
        access_token: result.access_token,
        refresh_token,
        expires_at: unix_time()?.saturating_add(result.expires_in),
    })
}

fn receive_oauth_code(
    listener: TcpListener,
    client_id: &str,
    redirect_uri: &str,
    expected_state: &str,
    verifier: &str,
) -> Result<(), String> {
    let deadline = std::time::Instant::now() + REDIRECT_TIMEOUT;
    let (mut stream, _) = loop {
        match listener.accept() {
            Ok(connection) => break connection,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if std::time::Instant::now() >= deadline {
                    return Err("Spotify sign-in timed out. Return to Coucou and try again.".into());
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(error) => return Err(format!("Could not receive Spotify callback: {error}")),
        }
    };
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .map_err(|error| format!("Could not configure Spotify callback request: {error}"))?;
    let mut buffer = [0_u8; 8192];
    let count = stream
        .read(&mut buffer)
        .map_err(|error| format!("Could not read Spotify callback: {error}"))?;
    let request = String::from_utf8_lossy(&buffer[..count]);
    let target = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .ok_or_else(|| "Spotify callback request was invalid.".to_string())?;
    let callback = Url::parse(&format!("http://127.0.0.1{target}"))
        .map_err(|error| format!("Spotify callback URL was invalid: {error}"))?;
    if callback.path() != "/callback" {
        return Err("Spotify sent an invalid callback path.".into());
    }
    let params: std::collections::HashMap<String, String> =
        callback.query_pairs().into_owned().collect();
    let response_text = "Spotify sign-in complete. You can close this tab and return to Coucou.";
    let (body, error) = if params.get("state").map(String::as_str) == Some(expected_state) {
        if let Some(code) = params.get("code") {
            let result = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| format!("Could not create Spotify sign-in runtime: {error}"))
                .and_then(|runtime| {
                    runtime.block_on(exchange_code(client_id, redirect_uri, code, verifier))
                });
            match result {
                Ok(()) => (response_text.to_string(), None),
                Err(error) => (
                    "Spotify sign-in failed. Return to Coucou and try again.".to_string(),
                    Some(error),
                ),
            }
        } else if let Some(reason) = params.get("error") {
            (
                "Spotify sign-in was not approved. Return to Coucou to try again.".to_string(),
                Some(format!("Spotify authorization was declined: {reason}")),
            )
        } else {
            (
                "Spotify did not return an authorization code. Return to Coucou and try again."
                    .to_string(),
                Some("Spotify did not return an authorization code.".to_string()),
            )
        }
    } else {
        (
            "Spotify sign-in could not be verified. Return to Coucou and try again.".to_string(),
            Some("Spotify sign-in state did not match.".to_string()),
        )
    };
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    stream
        .write_all(response.as_bytes())
        .map_err(|error| format!("Could not show Spotify sign-in result: {error}"))?;
    error.map_or(Ok(()), Err)
}

fn store_token(token: &OAuthToken) -> Result<(), String> {
    let value = serde_json::to_string(token)
        .map_err(|error| format!("Could not serialize Spotify credentials: {error}"))?;
    secrets::set(TOKEN_KEY, &value)
}

fn http_client() -> Result<Client, String> {
    Client::builder()
        .timeout(Duration::from_secs(12))
        .user_agent(concat!("Coucou/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|error| format!("Could not create Spotify HTTP client: {error}"))
}

async fn api_error(response: Response) -> String {
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    let message = serde_json::from_str::<Value>(&body)
        .ok()
        .and_then(|value| {
            value
                .get("error")
                .and_then(|error| error.get("message").or(Some(error)))
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
        .filter(|message| !message.is_empty())
        .unwrap_or_else(|| body.chars().take(400).collect());
    format!("Spotify API error {status}: {message}")
}

fn random_url_token(size: usize) -> String {
    let mut bytes = vec![0; size];
    rand::thread_rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

fn unix_time() -> Result<u64, String> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(|error| format!("System clock is invalid: {error}"))
}

#[cfg(test)]
mod tests {
    use super::{choose_playback_device, track_from_value};

    #[test]
    fn playback_actions_prefer_the_active_spotify_device() {
        let devices = serde_json::json!({
            "devices": [
                { "id": "inactive", "is_active": false, "is_restricted": false },
                { "id": "active", "is_active": true, "is_restricted": false }
            ]
        });

        assert_eq!(
            choose_playback_device(&devices, true).as_deref(),
            Some("active")
        );
    }

    #[test]
    fn play_can_target_an_available_device_when_none_is_active() {
        let devices = serde_json::json!({
            "devices": [
                { "id": "restricted", "is_active": false, "is_restricted": true },
                { "id": "available", "is_active": false, "is_restricted": false }
            ]
        });

        assert_eq!(
            choose_playback_device(&devices, true).as_deref(),
            Some("available")
        );
        assert_eq!(choose_playback_device(&devices, false), None);
    }

    #[test]
    fn ignores_devices_without_ids() {
        let devices = serde_json::json!({
            "devices": [
                { "is_active": true, "is_restricted": false },
                { "id": "", "is_active": true, "is_restricted": false }
            ]
        });

        assert_eq!(choose_playback_device(&devices, true), None);
    }

    #[test]
    fn maps_spotify_track_and_artwork_for_the_island() {
        let track = track_from_value(&serde_json::json!({
            "name": "Macha",
            "artists": [{ "name": "Coucou" }, { "name": "Mochi" }],
            "duration_ms": 187000,
            "uri": "spotify:track:123",
            "album": { "images": [{ "url": "https://i.scdn.co/image/cover" }] }
        }))
        .expect("track should be present");

        assert_eq!(track.name, "Macha");
        assert_eq!(track.artists, "Coucou, Mochi");
        assert_eq!(track.duration_ms, 187000);
        assert_eq!(track.uri.as_deref(), Some("spotify:track:123"));
        assert_eq!(
            track.image_url.as_deref(),
            Some("https://i.scdn.co/image/cover")
        );
    }

    #[test]
    fn ignores_incomplete_spotify_tracks() {
        assert!(track_from_value(&serde_json::json!({
            "name": "Incomplete",
            "artists": []
        }))
        .is_none());
    }
}
