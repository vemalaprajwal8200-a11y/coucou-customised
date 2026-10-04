// Claude API client — the same integration as ClaudeService.swift: multi-turn
// chat with web search, and files sent as document/image/text blocks.
//
// API requests and file reads stay on the Rust side; key reveal is a separate,
// explicit Settings action.

use std::collections::HashMap;
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
    conversations: Mutex<HashMap<String, Vec<Value>>>,
    active_openrouter_account: Mutex<Option<String>>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatHistoryMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SharedConversationContext {
    title: String,
    messages: Vec<ChatHistoryMessage>,
    file: Option<SharedContextFile>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SharedContextFile {
    name: String,
    path: String,
}

impl Chat {
    pub fn reset(&self) {
        self.conversations.lock().unwrap().clear();
    }

    pub fn delete(&self, conversation_id: &str) {
        self.conversations.lock().unwrap().remove(conversation_id);
    }

    fn seed_history(
        &self,
        conversation_id: &str,
        history: &[ChatHistoryMessage],
        context: Option<&ChatContext>,
        shared_context: &[SharedConversationContext],
    ) -> Result<(), String> {
        if conversation_id.is_empty() {
            return Err("Conversation ID is required.".into());
        }
        let mut conversations = self.conversations.lock().unwrap();
        let messages = conversations.entry(conversation_id.to_string()).or_default();
        if messages.is_empty() {
            let mut seeded = Vec::with_capacity(history.len());
            let mut context_added = false;
            for message in history {
                let role = match message.role.as_str() {
                    "user" => "user",
                    "assistant" => "assistant",
                    _ => return Err("Invalid conversation history.".into()),
                };
                if role == "user" {
                    let mut content = Vec::new();
                    if !context_added {
                        append_context(&mut content, context, shared_context)?;
                        context_added = true;
                    }
                    content.push(json!({ "type": "text", "text": message.content }));
                    seeded.push(json!({ "role": role, "content": content }));
                } else {
                    seeded.push(json!({
                        "role": role,
                        "content": [{ "type": "text", "text": message.content }],
                    }));
                }
            }
            *messages = seeded;
        }
        Ok(())
    }

    fn is_empty(&self, conversation_id: &str) -> bool {
        self.conversations
            .lock()
            .unwrap()
            .get(conversation_id)
            .map_or(true, Vec::is_empty)
    }

    fn push(&self, conversation_id: &str, message: Value) {
        self.conversations
            .lock()
            .unwrap()
            .entry(conversation_id.to_string())
            .or_default()
            .push(message);
    }

    fn pop(&self, conversation_id: &str) {
        if let Some(messages) = self.conversations.lock().unwrap().get_mut(conversation_id) {
            messages.pop();
        }
    }

    fn snapshot(&self, conversation_id: &str) -> Vec<Value> {
        self.conversations
            .lock()
            .unwrap()
            .get(conversation_id)
            .cloned()
            .unwrap_or_default()
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
    conversation_id: String,
    history: Vec<ChatHistoryMessage>,
    model: &str,
    query: String,
    context: Option<ChatContext>,
    shared_context: Vec<SharedConversationContext>,
) -> Result<ChatReply, String> {
    let accounts = secrets::openrouter_accounts()?;
    if accounts.is_empty() {
        return Err("No OpenRouter API keys are configured. Open Settings.".into());
    }

    chat.seed_history(&conversation_id, &history, context.as_ref(), &shared_context)?;

    // File / window context rides along with the first message only, exactly
    // like ClaudeService.chat().
    let mut content: Vec<Value> = Vec::new();
    let is_first_turn = chat.is_empty(&conversation_id);
    if is_first_turn {
        append_context(&mut content, context.as_ref(), &shared_context)?;
    }
    content.push(json!({ "type": "text", "text": query }));

    chat.push(&conversation_id, json!({ "role": "user", "content": content }));

    let body = request_body(model, chat.snapshot(&conversation_id));

    let current_id = chat.active_openrouter_account.lock().unwrap().clone();
    let start = current_id
        .as_ref()
        .and_then(|id| accounts.iter().position(|account| &account.id == id))
        .unwrap_or(0);
    let mut last_limit_error = None;
    let mut response = None;
    for offset in 0..accounts.len() {
        let index = (start + offset) % accounts.len();
        let account = &accounts[index];
        let key = match secrets::openrouter_key_for_account(&account.id) {
            Ok(key) => key,
            Err(error) => {
                chat.pop(&conversation_id);
                return Err(error);
            }
        };
        match call(&key, &body).await {
            Ok(value) => {
                *chat.active_openrouter_account.lock().unwrap() = Some(account.id.clone());
                response = Some(value);
                break;
            }
            Err(error) if error.account_limit_reached => {
                last_limit_error = Some(error.message);
                let next = &accounts[(index + 1) % accounts.len()];
                *chat.active_openrouter_account.lock().unwrap() = Some(next.id.clone());
            }
            Err(error) => {
                chat.pop(&conversation_id);
                return Err(error.message);
            }
        }
    }
    let Some(response) = response else {
        chat.pop(&conversation_id);
        return Err(format!(
            "All configured OpenRouter accounts have reached a usage or credit limit.{}",
            last_limit_error
                .map(|detail| format!(" Last error: {detail}"))
                .unwrap_or_default()
        ));
    };

    // A policy decline comes back as HTTP 200 with stop_reason "refusal".
    if response.get("stop_reason").and_then(Value::as_str) == Some("refusal") {
        chat.pop(&conversation_id);
        let why = response
            .get("stop_details")
            .and_then(|d| d.get("explanation"))
            .and_then(Value::as_str)
            .unwrap_or("Claude declined this one.");
        return Err(why.to_string());
    }

    let Some(blocks) = response.get("content").and_then(Value::as_array).cloned() else {
        chat.pop(&conversation_id);
        return Err("Unexpected API response.".into());
    };

    // Store the whole content — tool_use / tool_result blocks included — so the
    // next turn has the right context.
    chat.push(&conversation_id, json!({ "role": "assistant", "content": blocks.clone() }));

    let text = blocks
        .iter()
        .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|b| b.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string();

    if text.is_empty() {
        chat.pop(&conversation_id);
        chat.pop(&conversation_id);
        return Err("No response text.".into());
    }
    Ok(ChatReply { text })
}

fn append_context(
    content: &mut Vec<Value>,
    context: Option<&ChatContext>,
    shared_context: &[SharedConversationContext],
) -> Result<(), String> {
    for conversation in shared_context {
        content.push(json!({
            "type": "text",
            "text": format!(
                "Background from a previous conversation titled {:?}. Use it only when relevant.",
                conversation.title
            ),
        }));
        if let Some(file) = &conversation.file {
            match file_block(&file.path) {
                Ok(block) => content.push(block),
                Err(error) => content.push(json!({
                    "type": "text",
                    "text": format!("Previous attachment {:?} is unavailable: {error}", file.name),
                })),
            }
        }
        for message in &conversation.messages {
            content.push(json!({
                "type": "text",
                "text": format!(
                    "{}: {}",
                    if message.role == "user" { "User" } else { "Assistant" },
                    message.content
                ),
            }));
        }
    }
    match context {
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
    Ok(())
}

struct ApiFailure {
    message: String,
    account_limit_reached: bool,
}

async fn call(key: &str, body: &Value) -> Result<Value, ApiFailure> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(90))
        .build()
        .map_err(|e| ApiFailure {
            message: e.to_string(),
            account_limit_reached: false,
        })?;

    let response = client
        .post(ENDPOINT)
        .bearer_auth(key)
        .header("anthropic-version", ANTHROPIC_VERSION)
        .header("content-type", "application/json")
        .json(body)
        .send()
        .await
        .map_err(|e| ApiFailure {
            message: format!("Network error: {e}"),
            account_limit_reached: false,
        })?;

    let status = response.status();
    let text = response.text().await.map_err(|e| ApiFailure {
        message: e.to_string(),
        account_limit_reached: false,
    })?;
    if !status.is_success() {
        // Surface the API's own message, which is what makes a bad key obvious.
        let parsed = serde_json::from_str::<Value>(&text).ok();
        let detail = parsed
            .as_ref()
            .and_then(|value| value.get("error"))
            .and_then(|error| error.get("message"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| text.chars().take(200).collect());
        let code = parsed
            .as_ref()
            .and_then(|value| value.get("error"))
            .and_then(|error| error.get("code"))
            .and_then(Value::as_i64);
        let account_limit_reached = is_account_limit(status, code, &detail);
        return Err(ApiFailure {
            message: format!("OpenRouter API {status}: {detail}"),
            account_limit_reached,
        });
    }

    serde_json::from_str(&text).map_err(|e| ApiFailure {
        message: format!("Bad API response: {e}"),
        account_limit_reached: false,
    })
}

fn is_account_limit(status: reqwest::StatusCode, code: Option<i64>, detail: &str) -> bool {
    let detail = detail.to_ascii_lowercase();
    status == reqwest::StatusCode::PAYMENT_REQUIRED
        || code == Some(402)
        || status == reqwest::StatusCode::TOO_MANY_REQUESTS
        || code == Some(429)
        || [
            "insufficient credits",
            "not enough credits",
            "out of credits",
            "credit balance",
            "quota exceeded",
            "budget exceeded",
        ]
        .iter()
        .any(|phrase| detail.contains(phrase))
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
    use super::{
        append_context, base64, file_block, is_account_limit, openrouter_model, request_body,
        Chat, ChatContext, ChatHistoryMessage, SharedContextFile, SharedConversationContext,
        DEFAULT_MODEL,
    };
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
    fn credit_and_rate_limit_errors_trigger_key_failover() {
        assert!(is_account_limit(
            reqwest::StatusCode::PAYMENT_REQUIRED,
            None,
            "Payment required",
        ));
        assert!(is_account_limit(
            reqwest::StatusCode::BAD_REQUEST,
            Some(402),
            "Insufficient credits",
        ));
        assert!(is_account_limit(
            reqwest::StatusCode::FORBIDDEN,
            None,
            "Account has insufficient credits",
        ));
        assert!(is_account_limit(
            reqwest::StatusCode::TOO_MANY_REQUESTS,
            None,
            "Rate limit exceeded: free-models-per-day. Add 10 credits to unlock 1000 free model requests per day",
        ));
        assert!(is_account_limit(
            reqwest::StatusCode::TOO_MANY_REQUESTS,
            Some(429),
            "Too many requests",
        ));
        assert!(!is_account_limit(
            reqwest::StatusCode::UNAUTHORIZED,
            None,
            "Invalid API key",
        ));
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

    #[test]
    fn conversation_histories_are_isolated_and_can_be_restored() {
        let chat = Chat::default();
        let history = vec![
            ChatHistoryMessage { role: "user".into(), content: "Summarize this".into() },
            ChatHistoryMessage { role: "assistant".into(), content: "A short summary".into() },
        ];

        chat.seed_history("first", &history, None, &[]).unwrap();
        chat.seed_history("second", &[], None, &[]).unwrap();

        assert_eq!(chat.snapshot("first").len(), 2);
        assert!(chat.snapshot("second").is_empty());

        chat.seed_history("first", &[], None, &[]).unwrap();
        assert_eq!(chat.snapshot("first").len(), 2);
        assert!(chat.seed_history(
            "invalid",
            &[ChatHistoryMessage { role: "system".into(), content: "no".into() }],
            None,
            &[],
        ).is_err());
    }

    #[test]
    fn deleting_conversation_clears_only_its_model_history() {
        let chat = Chat::default();
        chat.push("first", serde_json::json!({ "role": "user", "content": [] }));
        chat.push("second", serde_json::json!({ "role": "user", "content": [] }));

        chat.delete("first");

        assert!(chat.snapshot("first").is_empty());
        assert_eq!(chat.snapshot("second").len(), 1);
    }

    #[test]
    fn restored_file_conversation_reloads_its_attachment_context() {
        let dir = std::env::temp_dir().join(format!("coucou-restore-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("note.txt");
        fs::write(&path, "private local file contents").unwrap();

        let chat = Chat::default();
        let history = [ChatHistoryMessage { role: "user".into(), content: "What is this?".into() }];
        chat.seed_history(
            "file-conversation",
            &history,
            Some(&ChatContext::File {
                name: "note.txt".into(),
                path: path.to_string_lossy().into_owned(),
            }),
            &[SharedConversationContext {
                title: "earlier".into(),
                messages: vec![ChatHistoryMessage {
                    role: "user".into(),
                    content: "remember this".into(),
                }],
                file: None,
            }],
        )
        .unwrap();
        let seeded = chat.snapshot("file-conversation");
        assert!(seeded[0]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("earlier"));
        assert_eq!(seeded[0]["content"][1]["text"], "User: remember this");
        assert_eq!(seeded[0]["content"][2]["text"], "File contents:\nprivate local file contents");
        assert_eq!(seeded[0]["content"][3]["text"], "File: note.txt");
        assert_eq!(seeded[0]["content"][4]["text"], "What is this?");

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn shared_context_includes_previous_messages_and_file_contents() {
        let dir = std::env::temp_dir().join(format!("coucou-shared-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("reference.txt");
        fs::write(&path, "shared attachment facts").unwrap();

        let previous = [SharedConversationContext {
            title: "Reference chat".into(),
            messages: vec![
                ChatHistoryMessage { role: "user".into(), content: "Remember the project name".into() },
                ChatHistoryMessage { role: "assistant".into(), content: "The project is Mochi".into() },
            ],
            file: Some(SharedContextFile {
                name: "reference.txt".into(),
                path: path.to_string_lossy().into_owned(),
            }),
        }];
        let mut blocks = Vec::new();
        append_context(&mut blocks, None, &previous).unwrap();

        assert!(blocks.iter().any(|block| block["text"] == "User: Remember the project name"));
        assert!(blocks.iter().any(|block| block["text"] == "Assistant: The project is Mochi"));
        assert!(blocks.iter().any(|block| block["text"] == "File contents:\nshared attachment facts"));

        fs::remove_dir_all(&dir).unwrap();
    }
}
