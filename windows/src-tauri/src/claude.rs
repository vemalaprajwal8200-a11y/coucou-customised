// Claude API client — the same integration as ClaudeService.swift: multi-turn
// chat with web search, and files sent as document/image/text blocks.
//
// Everything happens here rather than in the island: the API key never leaves
// the Credential Manager, and file bytes never cross the IPC boundary.

use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::secrets;

const ENDPOINT: &str = "https://openrouter.ai/api/v1/messages";
const ANTHROPIC_VERSION: &str = "2023-06-01";
const MAX_TOKENS: u32 = 4096;
/// Text and code files are inlined; anything larger is skipped, as on macOS.
const MAX_INLINE_TEXT: u64 = 200_000;

pub const DEFAULT_MODEL: &str = "nvidia/nemotron-3-super-120b-a12b:free";

const SYSTEM_PROMPT: &str = "You are Mochi, a personal AI assistant living at the top of the user's screen. \
You have web search access and can help with absolutely anything — research, coding, finding places, recommendations, tasks, questions. \
Respond in the user's language. Be thorough and complete — use as much detail as the task requires. \
No markdown formatting (no **, no ##, no bullet dashes). Use plain text with line breaks.";

#[derive(Default)]
pub struct Chat {
    /// Full multi-turn history, including tool_use / tool_result blocks.
    messages: Mutex<Vec<Value>>,
}

impl Chat {
    pub fn reset(&self) {
        self.messages.lock().unwrap().clear();
    }

    fn is_empty(&self) -> bool {
        self.messages.lock().unwrap().is_empty()
    }

    fn push(&self, message: Value) {
        self.messages.lock().unwrap().push(message);
    }

    fn pop(&self) {
        self.messages.lock().unwrap().pop();
    }

    fn snapshot(&self) -> Vec<Value> {
        self.messages.lock().unwrap().clone()
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ChatContext {
    File { name: String, path: String },
    Window { app_name: String, title: String, url: Option<String> },
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatReply {
    pub text: String,
}

/// One chat turn. Returns the assistant's text, or a message the island shows
/// in the note view.
pub async fn send(
    chat: &Chat,
    model: &str,
    query: String,
    context: Option<ChatContext>,
) -> Result<ChatReply, String> {
    let key = secrets::get("openrouter-api-key")
        .ok_or_else(|| "API key missing. Open settings.".to_string())?;

    let mut content: Vec<Value> = Vec::new();

    // File / window context rides along with the first message only, exactly
    // like ClaudeService.chat().
    if chat.is_empty() {
        match &context {
            Some(ChatContext::File { name, path }) => {
                content.push(file_block(path)?);
                content.push(json!({ "type": "text", "text": format!("File: {name}") }));
            }
            Some(ChatContext::Window { app_name, title, url }) => {
                let mut text = format!("Context — App: {app_name}, Window: {title}");
                if let Some(url) = url {
                    text.push_str(&format!(", URL: {url}"));
                }
                content.push(json!({ "type": "text", "text": text }));
            }
            None => {}
        }
    }
    content.push(json!({ "type": "text", "text": query }));

    chat.push(json!({ "role": "user", "content": content }));

    let body = request_body(model, chat.snapshot());

    let response = match call(&key, &body).await {
        Ok(v) => v,
        Err(err) => {
            chat.pop(); // keep the history consistent with what the model saw
            return Err(err);
        }
    };

    // A policy decline comes back as HTTP 200 with stop_reason "refusal".
    if response.get("stop_reason").and_then(Value::as_str) == Some("refusal") {
        chat.pop();
        let why = response
            .get("stop_details")
            .and_then(|d| d.get("explanation"))
            .and_then(Value::as_str)
            .unwrap_or("Claude declined this one.");
        return Err(why.to_string());
    }

    let Some(blocks) = response.get("content").and_then(Value::as_array).cloned() else {
        chat.pop();
        return Err("Unexpected API response.".into());
    };

    // Store the whole content — tool_use / tool_result blocks included — so the
    // next turn has the right context.
    chat.push(json!({ "role": "assistant", "content": blocks.clone() }));

    let text = blocks
        .iter()
        .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|b| b.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string();

    if text.is_empty() {
        return Err("No response text.".into());
    }
    Ok(ChatReply { text })
}

async fn call(key: &str, body: &Value) -> Result<Value, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(90))
        .build()
        .map_err(|e| e.to_string())?;

    let response = client
        .post(ENDPOINT)
        .bearer_auth(key)
        .header("anthropic-version", ANTHROPIC_VERSION)
        .header("content-type", "application/json")
        .json(body)
        .send()
        .await
        .map_err(|e| format!("Network error: {e}"))?;

    let status = response.status();
    let text = response.text().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        // Surface the API's own message, which is what makes a bad key obvious.
        let detail = serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|v| {
                v.get("error")
                    .and_then(|e| e.get("message"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .unwrap_or_else(|| text.chars().take(200).collect());
        return Err(format!("OpenRouter API {status}: {detail}"));
    }
    serde_json::from_str(&text).map_err(|e| format!("Bad API response: {e}"))
}

fn openrouter_model(model: &str) -> String {
    let slug = match model {
        "claude-haiku-4-5" => "claude-haiku-4.5",
        other => other,
    };
    if slug.contains('/') {
        slug.to_string()
    } else {
        format!("anthropic/{slug}")
    }
}

fn request_body(model: &str, messages: Vec<Value>) -> Value {
    json!({
        "model": openrouter_model(model),
        "max_tokens": MAX_TOKENS,
        "system": SYSTEM_PROMPT,
        "tools": [{
            "type": "openrouter:web_search",
            "parameters": { "max_uses": 5 },
        }],
        "messages": messages,
    })
}

/// PDF → document block, image → image block, text/code → inline text.
/// Mirrors readFileAsBlock() in ClaudeService.swift.
fn file_block(path: &str) -> Result<Value, String> {
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    let media_type = match ext.as_str() {
        "pdf" => Some(("document", "application/pdf")),
        "jpg" | "jpeg" => Some(("image", "image/jpeg")),
        "png" => Some(("image", "image/png")),
        "gif" => Some(("image", "image/gif")),
        "webp" => Some(("image", "image/webp")),
        _ => None,
    };

    if let Some((block_type, media)) = media_type {
        let bytes = std::fs::read(path).map_err(|e| format!("Cannot read attachment: {e}"))?;
        return Ok(json!({
            "type": block_type,
            "source": { "type": "base64", "media_type": media, "data": base64(&bytes) },
        }));
    }

    let len = std::fs::metadata(path)
        .map_err(|e| format!("Cannot read attachment: {e}"))?
        .len();
    if len > MAX_INLINE_TEXT {
        return Err(format!(
            "This text file is too large to attach ({} KB maximum).",
            MAX_INLINE_TEXT / 1024
        ));
    }
    let text = std::fs::read_to_string(path)
        .map_err(|_| "This file is not a supported text, PDF, or image file.".to_string())?;
    Ok(json!({ "type": "text", "text": format!("File contents:\n{text}") }))
}

/// Small standalone base64 encoder — not worth another dependency.
/// Also used for Stripe's basic auth.
pub(crate) fn base64_for(bytes: &[u8]) -> String {
    base64(bytes)
}

fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { TABLE[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { TABLE[n as usize & 63] as char } else { '=' });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{base64, file_block, openrouter_model, request_body, DEFAULT_MODEL};
    use std::fs;

    #[test]
    fn request_uses_nemotron_free_and_openrouter_search() {
        let body = request_body(DEFAULT_MODEL, Vec::new());

        assert_eq!(body["model"], DEFAULT_MODEL);
        assert_eq!(body["tools"][0]["type"], "openrouter:web_search");
        assert_eq!(body["tools"][0]["parameters"]["max_uses"], 5);
    }

    #[test]
    fn openrouter_model_uses_provider_slugs() {
        assert_eq!(openrouter_model("claude-opus-5"), "anthropic/claude-opus-5");
        assert_eq!(openrouter_model("claude-sonnet-5"), "anthropic/claude-sonnet-5");
        assert_eq!(openrouter_model("claude-haiku-4-5"), "anthropic/claude-haiku-4.5");
        assert_eq!(openrouter_model("other/provider-model"), "other/provider-model");
    }

    #[test]
    fn base64_matches_rfc4648_vectors() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
        assert_eq!(base64(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn file_block_reads_text_and_encodes_images() {
        let dir = std::env::temp_dir().join(format!("coucou-chat-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();

        let text_path = dir.join("note.txt");
        fs::write(&text_path, "hello").unwrap();
        let text = file_block(text_path.to_str().unwrap()).unwrap();
        assert_eq!(text["type"], "text");
        assert_eq!(text["text"], "File contents:\nhello");

        let image_path = dir.join("pixel.png");
        fs::write(&image_path, b"png bytes").unwrap();
        let image = file_block(image_path.to_str().unwrap()).unwrap();
        assert_eq!(image["type"], "image");
        assert_eq!(image["source"]["media_type"], "image/png");
        assert_eq!(image["source"]["data"], base64(b"png bytes"));

        let binary_path = dir.join("document.docx");
        fs::write(&binary_path, [0xff, 0xfe]).unwrap();
        assert!(file_block(binary_path.to_str().unwrap())
            .unwrap_err()
            .contains("not a supported text"));

        let missing_path = dir.join("missing.txt");
        assert!(file_block(missing_path.to_str().unwrap())
            .unwrap_err()
            .contains("Cannot read attachment"));

        fs::remove_dir_all(&dir).unwrap();
    }
}
