// Claude API client — the same integration as ClaudeService.swift: multi-turn
// chat with web search, and files sent as text, image, or file content parts.
//
// API requests and file reads stay on the Rust side; key reveal is a separate,
// explicit Settings action.

use std::collections::HashMap;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::watch;

use crate::{automation, llm_client};

/// Text and code files are inlined; anything larger is skipped, as on macOS.
const MAX_INLINE_TEXT: u64 = 200_000;
const MAX_AUTOMATION_ACTIONS: u8 = 5;

pub const DEFAULT_MODEL: &str = "openrouter/free";

const SYSTEM_PROMPT: &str = "You are Mochi, a personal AI assistant living at the top of the user's screen. \
You have web search access and can help with absolutely anything — research, coding, finding places, recommendations, tasks, questions. \
When the user explicitly asks to open an installed app, folder, or file, use the local action tools instead of merely explaining how. \
For file edits, write_file replaces all existing contents and erase_file_content clears them; state that clearly and wait for the user's approval. \
Respond in the user's language. Be thorough and complete — use as much detail as the task requires. \
Use fenced code blocks for code, commands, configuration, and other copyable snippets; add a language tag when known. \
Keep surrounding prose plain and readable.";

#[derive(Default)]
pub struct Chat {
    /// Full multi-turn history in OpenRouter Chat Completions message format.
    conversations: Mutex<HashMap<String, Vec<Value>>>,
    pending_actions: Mutex<HashMap<String, PendingAction>>,
    action_counts: Mutex<HashMap<String, u8>>,
    requests: Mutex<HashMap<String, watch::Sender<bool>>>,
}

struct PendingAction {
    tool_call_id: Option<String>,
    action: automation::AutomationAction,
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
        self.pending_actions.lock().unwrap().clear();
        self.action_counts.lock().unwrap().clear();
        for request in self.requests.lock().unwrap().values() {
            let _ = request.send(true);
        }
        self.requests.lock().unwrap().clear();
    }

    pub fn delete(&self, conversation_id: &str) {
        self.conversations.lock().unwrap().remove(conversation_id);
        self.pending_actions.lock().unwrap().remove(conversation_id);
        self.action_counts.lock().unwrap().remove(conversation_id);
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
        let messages = conversations
            .entry(conversation_id.to_string())
            .or_default();
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
                        "content": message.content,
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

    fn take_pending_action(&self, conversation_id: &str) -> Option<PendingAction> {
        self.pending_actions.lock().unwrap().remove(conversation_id)
    }

    fn action_count(&self, conversation_id: &str) -> u8 {
        self.action_counts
            .lock()
            .unwrap()
            .get(conversation_id)
            .copied()
            .unwrap_or_default()
    }

    fn increment_action_count(&self, conversation_id: &str) {
        let mut counts = self.action_counts.lock().unwrap();
        *counts.entry(conversation_id.to_string()).or_default() += 1;
    }

    fn reset_action_count(&self, conversation_id: &str) {
        self.action_counts.lock().unwrap().remove(conversation_id);
    }

    pub fn begin_request(&self, request_id: &str) -> Result<watch::Receiver<bool>, String> {
        if request_id.is_empty() || request_id.len() > 100 {
            return Err("Invalid chat request ID.".into());
        }
        let (sender, receiver) = watch::channel(false);
        let mut requests = self.requests.lock().unwrap();
        if requests.contains_key(request_id) {
            return Err("A chat request with this ID is already running.".into());
        }
        requests.insert(request_id.to_string(), sender);
        Ok(receiver)
    }

    pub fn cancel_request(&self, request_id: &str) -> Result<(), String> {
        let requests = self.requests.lock().unwrap();
        let sender = requests
            .get(request_id)
            .ok_or_else(|| "This chat request is no longer running.".to_string())?;
        sender
            .send(true)
            .map_err(|_| "This chat request is no longer running.".to_string())
    }

    pub fn finish_request(&self, request_id: &str) {
        self.requests.lock().unwrap().remove(request_id);
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ChatContext {
    File {
        name: String,
        path: String,
    },
    Window {
        app_name: String,
        title: String,
        url: Option<String>,
    },
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatReply {
    pub text: String,
    pub model: String,
    pub action: Option<automation::AutomationAction>,
    pub provider: String,
    pub fallback_notice: Option<String>,
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
    automation_folders: &[String],
    provider_mode: &str,
    ollama_model: &str,
    cancel: &mut watch::Receiver<bool>,
) -> Result<ChatReply, String> {
    chat.seed_history(
        &conversation_id,
        &history,
        context.as_ref(),
        &shared_context,
    )?;
    chat.reset_action_count(&conversation_id);

    if let Some(app_name) = automation::explicit_open_app(&query) {
        let action = automation::AutomationAction {
            name: "open_app".into(),
            arguments: json!({ "appName": app_name }),
            preview: None,
            preview_hash: None,
        };
        return propose_local_action(chat, &conversation_id, &query, action, automation_folders);
    }
    if let Some(path) = automation::explicit_open_path(&query) {
        let action = automation::AutomationAction {
            name: "open_path".into(),
            arguments: json!({ "path": path }),
            preview: None,
            preview_hash: None,
        };
        return propose_local_action(chat, &conversation_id, &query, action, automation_folders);
    }

    // File / window context rides along with the first message only, exactly
    // like ClaudeService.chat().
    let mut content: Vec<Value> = Vec::new();
    let is_first_turn = chat.is_empty(&conversation_id);
    if is_first_turn {
        append_context(&mut content, context.as_ref(), &shared_context)?;
    }
    content.push(json!({ "type": "text", "text": query }));

    chat.push(
        &conversation_id,
        json!({ "role": "user", "content": content }),
    );

    let completion = match llm_client::chat(
        provider_messages(chat.snapshot(&conversation_id), automation_folders),
        (!automation_folders.is_empty()).then(automation_tools),
        provider_mode,
        ollama_model,
        model,
        cancel,
    )
    .await
    {
        Ok(completion) => completion,
        Err(error) => {
            chat.pop(&conversation_id);
            return Err(error);
        }
    };

    match handle_response(chat, &conversation_id, automation_folders, completion) {
        Ok(reply) => Ok(reply),
        Err(error) => {
            chat.pop(&conversation_id);
            Err(error)
        }
    }
}

fn propose_local_action(
    chat: &Chat,
    conversation_id: &str,
    query: &str,
    action: automation::AutomationAction,
    automation_folders: &[String],
) -> Result<ChatReply, String> {
    let action = automation::prepare(action, automation_folders)?;
    chat.push(conversation_id, json!({ "role": "user", "content": query }));
    chat.pending_actions.lock().unwrap().insert(
        conversation_id.to_string(),
        PendingAction {
            tool_call_id: None,
            action: action.clone(),
        },
    );
    Ok(ChatReply {
        text: String::new(),
        model: "Local automation".into(),
        action: Some(action),
        provider: "local".into(),
        fallback_notice: None,
    })
}

pub async fn resolve_action(
    chat: &Chat,
    conversation_id: &str,
    model: &str,
    approved: bool,
    selected_app_id: Option<String>,
    automation_folders: &[String],
    provider_mode: &str,
    ollama_model: &str,
    cancel: &mut watch::Receiver<bool>,
) -> Result<ChatReply, String> {
    if approved {
        let mut pending_actions = chat.pending_actions.lock().unwrap();
        let pending = pending_actions
            .get_mut(conversation_id)
            .ok_or_else(|| "This local action is no longer pending.".to_string())?;
        automation::select_app_target(&mut pending.action, selected_app_id.as_deref())?;
    }
    let pending = chat
        .take_pending_action(conversation_id)
        .ok_or_else(|| "This local action is no longer pending.".to_string())?;
    let Some(tool_call_id) = pending.tool_call_id else {
        let text = if approved {
            automation::execute(&pending.action, automation_folders)?
        } else if pending.action.name == "open_app" {
            "Okay, I won't open the app. Nothing was changed.".into()
        } else if pending.action.name == "open_path" {
            "Okay, I won't open the file or folder. Nothing was changed.".into()
        } else {
            "Okay, I won't perform that action. Nothing was changed.".into()
        };
        chat.push(
            conversation_id,
            json!({ "role": "assistant", "content": text }),
        );
        return Ok(ChatReply {
            text,
            model: "Local automation".into(),
            action: None,
            provider: "local".into(),
            fallback_notice: None,
        });
    };
    let result = if approved {
        automation::execute(&pending.action, automation_folders)
            .unwrap_or_else(|error| format!("Action failed: {error}"))
    } else {
        "The user denied this action. Do not repeat it; explain that no change was made.".into()
    };
    chat.push(
        conversation_id,
        json!({
            "role": "tool",
            "tool_call_id": tool_call_id,
            "content": result,
        }),
    );
    let completion = llm_client::chat(
        provider_messages(chat.snapshot(conversation_id), automation_folders),
        (!automation_folders.is_empty()).then(automation_tools),
        provider_mode,
        ollama_model,
        model,
        cancel,
    )
    .await?;
    handle_response(chat, conversation_id, automation_folders, completion)
}

fn provider_messages(mut history: Vec<Value>, automation_folders: &[String]) -> Vec<Value> {
    const MAX_HISTORY_MESSAGES: usize = 20;
    if history.len() > MAX_HISTORY_MESSAGES {
        history.drain(..history.len() - MAX_HISTORY_MESSAGES);
    }
    let automation_context = if automation_folders.is_empty() {
        String::new()
    } else {
        format!(
            "\nThe user authorized local file actions only inside these folders: {}. \
Never attempt deletion, command execution, or actions outside those folders. \
Local actions require explicit user approval; request one action at a time.",
            automation_folders.join(", ")
        )
    };
    let mut messages = vec![json!({
        "role": "system",
        "content": format!("{SYSTEM_PROMPT}{automation_context}"),
    })];
    messages.extend(history);
    messages
}

fn handle_response(
    chat: &Chat,
    conversation_id: &str,
    automation_folders: &[String],
    completion: llm_client::ChatReply,
) -> Result<ChatReply, String> {
    let response = completion.response;
    let Some(message) = response
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .and_then(|choice| choice.get("message"))
    else {
        return Err("Unexpected API response.".into());
    };
    let used_model = completion.model;
    if let Some(refusal) = message.get("refusal").and_then(Value::as_str) {
        return Err(refusal.to_string());
    }
    if let Some(tool_calls) = message.get("tool_calls").and_then(Value::as_array) {
        if tool_calls.len() != 1 {
            return Err("The assistant returned an unsupported number of local actions.".into());
        }
        if automation_folders.is_empty() {
            return Err("No folders are authorized for local automation.".into());
        }
        let tool_call = &tool_calls[0];
        let tool_call_id = tool_call
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .ok_or_else(|| "The assistant returned an invalid local action.".to_string())?;
        let function = tool_call
            .get("function")
            .ok_or_else(|| "The assistant returned an invalid local action.".to_string())?;
        let name = function
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| automation::supports(name))
            .ok_or_else(|| "The assistant requested an unsupported local action.".to_string())?;
        let arguments = function
            .get("arguments")
            .and_then(Value::as_str)
            .ok_or_else(|| "The assistant returned invalid action arguments.".to_string())?;
        let arguments = serde_json::from_str(arguments)
            .map_err(|e| format!("The assistant returned invalid action arguments: {e}"))?;
        let action = automation::prepare(
            automation::AutomationAction {
                name: name.to_string(),
                arguments,
                preview: None,
                preview_hash: None,
            },
            automation_folders,
        )?;
        if chat.action_count(conversation_id) >= MAX_AUTOMATION_ACTIONS {
            let text = "For safety, I stopped after five local actions. Ask me to continue if you need more.".to_string();
            chat.push(
                conversation_id,
                json!({ "role": "assistant", "content": text }),
            );
            chat.reset_action_count(conversation_id);
            return Ok(ChatReply {
                text,
                model: used_model,
                action: None,
                provider: completion.provider,
                fallback_notice: completion.fallback_notice,
            });
        }
        chat.push(conversation_id, message.clone());
        chat.pending_actions.lock().unwrap().insert(
            conversation_id.to_string(),
            PendingAction {
                tool_call_id: Some(tool_call_id.to_string()),
                action: action.clone(),
            },
        );
        chat.increment_action_count(conversation_id);
        return Ok(ChatReply {
            text: message
                .get("content")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim()
                .to_string(),
            model: used_model,
            action: Some(action),
            provider: completion.provider,
            fallback_notice: completion.fallback_notice,
        });
    }
    let text = response_text(message);

    if text.is_empty() {
        return Err("No response text.".into());
    }
    chat.push(
        conversation_id,
        json!({ "role": "assistant", "content": text }),
    );
    chat.reset_action_count(conversation_id);
    Ok(ChatReply {
        text,
        model: used_model,
        action: None,
        provider: completion.provider,
        fallback_notice: completion.fallback_notice,
    })
}

fn response_text(message: &Value) -> String {
    let content = message
        .get("content")
        .and_then(value_text)
        .unwrap_or_default();
    if !content.trim().is_empty() {
        return content.trim().to_string();
    }
    message
        .get("reasoning")
        .or_else(|| message.get("reasoning_content"))
        .and_then(value_text)
        .unwrap_or_default()
        .trim()
        .to_string()
}

fn value_text(value: &Value) -> Option<String> {
    value.as_str().map(str::to_string).or_else(|| {
        value.as_array().map(|parts| {
            parts
                .iter()
                .filter_map(|part| part.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n")
        })
    })
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
        Some(ChatContext::Window {
            app_name,
            title,
            url,
        }) => {
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

fn automation_tools() -> Value {
    json!([
        {
            "type": "function",
            "function": {
                "name": "open_app",
                "description": "Open an installed app by its exact Windows Start menu shortcut name. Requires explicit user approval.",
                "parameters": {
                    "type": "object",
                    "properties": { "appName": { "type": "string" } },
                    "required": ["appName"],
                    "additionalProperties": false
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "list_directory",
                "description": "List names and types in a directory inside an authorized folder.",
                "parameters": {
                    "type": "object",
                    "properties": { "path": { "type": "string", "description": "Absolute authorized path or path relative to an authorized folder." } },
                    "required": ["path"],
                    "additionalProperties": false
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "read_file",
                "description": "Read a small UTF-8 text file inside an authorized folder. The contents are sent to the configured AI provider.",
                "parameters": {
                    "type": "object",
                    "properties": { "path": { "type": "string" } },
                    "required": ["path"],
                    "additionalProperties": false
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "open_path",
                "description": "Open a folder or file from an authorized folder in the system file manager.",
                "parameters": {
                    "type": "object",
                    "properties": { "path": { "type": "string" } },
                    "required": ["path"],
                    "additionalProperties": false
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "create_directory",
                "description": "Create one new folder inside an authorized folder. Does not create parents or overwrite.",
                "parameters": {
                    "type": "object",
                    "properties": { "path": { "type": "string" } },
                    "required": ["path"],
                    "additionalProperties": false
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "create_file",
                "description": "Create a new UTF-8 text file inside an authorized folder. Fails if the file already exists.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" },
                        "content": { "type": "string" }
                    },
                    "required": ["path", "content"],
                    "additionalProperties": false
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "write_file",
                "description": "Replace all contents of an existing UTF-8 text file inside an authorized folder. Show the proposed contents and get explicit user approval first. A backup is created.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" },
                        "content": { "type": "string" }
                    },
                    "required": ["path", "content"],
                    "additionalProperties": false
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "erase_file_content",
                "description": "Clear all contents of an existing file inside an authorized folder. Show the current contents and get explicit user approval first. A backup is created.",
                "parameters": {
                    "type": "object",
                    "properties": { "path": { "type": "string" } },
                    "required": ["path"],
                    "additionalProperties": false
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "move_file",
                "description": "Move a file to a new path on the same filesystem inside an authorized folder. Does not overwrite existing files.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" },
                        "destination": { "type": "string" }
                    },
                    "required": ["path", "destination"],
                    "additionalProperties": false
                }
            }
        }
    ])
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
        let data_url = format!("data:{media};base64,{}", base64(&bytes));
        return Ok(if block_type == "image" {
            json!({
                "type": "image_url",
                "image_url": { "url": data_url },
            })
        } else {
            json!({
                "type": "file",
                "file": {
                    "filename": std::path::Path::new(path)
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or("attachment.pdf"),
                    "file_data": data_url,
                },
            })
        });
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
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            TABLE[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{
        append_context, base64, file_block, handle_response, provider_messages, response_text,
        Chat, ChatContext, ChatHistoryMessage, SharedContextFile, SharedConversationContext,
    };
    use crate::llm_client::ChatReply as LlmReply;
    use std::fs;

    #[test]
    fn tool_call_is_returned_as_a_pending_user_approval() {
        let chat = Chat::default();
        let response = serde_json::json!({
            "model": "test/model",
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call-1",
                        "type": "function",
                        "function": {
                            "name": "list_directory",
                            "arguments": "{\"path\":\"notes\"}"
                        }
                    }]
                }
            }]
        });
        let folder = std::env::temp_dir().join(format!("coucou-tool-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        std::fs::create_dir_all(folder.join("notes")).unwrap();
        let folders = vec![folder.to_string_lossy().into_owned()];
        let reply = handle_response(
            &chat,
            "conversation",
            &folders,
            LlmReply {
                response,
                provider: "ollama".into(),
                model: "qwen2.5:7b".into(),
                fallback_notice: None,
            },
        )
        .unwrap();
        assert_eq!(reply.action.as_ref().unwrap().name, "list_directory");
        assert!(chat
            .pending_actions
            .lock()
            .unwrap()
            .contains_key("conversation"));
        std::fs::remove_dir_all(folder).unwrap();
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
        assert_eq!(image["type"], "image_url");
        assert_eq!(
            image["image_url"]["url"],
            format!("data:image/png;base64,{}", base64(b"png bytes"))
        );

        let pdf_path = dir.join("document.pdf");
        fs::write(&pdf_path, b"%PDF test").unwrap();
        let pdf = file_block(pdf_path.to_str().unwrap()).unwrap();
        assert_eq!(pdf["type"], "file");
        assert_eq!(pdf["file"]["filename"], "document.pdf");
        assert_eq!(
            pdf["file"]["file_data"],
            format!("data:application/pdf;base64,{}", base64(b"%PDF test"))
        );

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
            ChatHistoryMessage {
                role: "user".into(),
                content: "Summarize this".into(),
            },
            ChatHistoryMessage {
                role: "assistant".into(),
                content: "A short summary".into(),
            },
        ];

        chat.seed_history("first", &history, None, &[]).unwrap();
        chat.seed_history("second", &[], None, &[]).unwrap();

        assert_eq!(chat.snapshot("first").len(), 2);
        assert!(chat.snapshot("second").is_empty());

        chat.seed_history("first", &[], None, &[]).unwrap();
        assert_eq!(chat.snapshot("first").len(), 2);
        assert!(chat
            .seed_history(
                "invalid",
                &[ChatHistoryMessage {
                    role: "system".into(),
                    content: "no".into()
                }],
                None,
                &[],
            )
            .is_err());
    }

    #[test]
    fn deleting_conversation_clears_only_its_model_history() {
        let chat = Chat::default();
        chat.push(
            "first",
            serde_json::json!({ "role": "user", "content": [] }),
        );
        chat.push(
            "second",
            serde_json::json!({ "role": "user", "content": [] }),
        );

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
        let history = [ChatHistoryMessage {
            role: "user".into(),
            content: "What is this?".into(),
        }];
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
        assert_eq!(
            seeded[0]["content"][2]["text"],
            "File contents:\nprivate local file contents"
        );
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
                ChatHistoryMessage {
                    role: "user".into(),
                    content: "Remember the project name".into(),
                },
                ChatHistoryMessage {
                    role: "assistant".into(),
                    content: "The project is Mochi".into(),
                },
            ],
            file: Some(SharedContextFile {
                name: "reference.txt".into(),
                path: path.to_string_lossy().into_owned(),
            }),
        }];
        let mut blocks = Vec::new();
        append_context(&mut blocks, None, &previous).unwrap();

        assert!(blocks
            .iter()
            .any(|block| block["text"] == "User: Remember the project name"));
        assert!(blocks
            .iter()
            .any(|block| block["text"] == "Assistant: The project is Mochi"));
        assert!(blocks
            .iter()
            .any(|block| block["text"] == "File contents:\nshared attachment facts"));

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn final_answer_content_takes_precedence_over_reasoning() {
        let message = serde_json::json!({
            "content": "Final answer",
            "reasoning": "private reasoning",
            "reasoning_content": "also private",
        });
        assert_eq!(response_text(&message), "Final answer");
    }

    #[test]
    fn reasoning_is_used_only_when_no_final_content_exists() {
        let message = serde_json::json!({
            "content": "",
            "reasoning_content": "fallback content",
        });
        assert_eq!(response_text(&message), "fallback content");
    }

    #[test]
    fn provider_history_keeps_system_prompt_and_only_recent_twenty_messages() {
        let history = (0..25)
            .map(|index| {
                serde_json::json!({
                    "role": if index % 2 == 0 { "user" } else { "assistant" },
                    "content": index.to_string(),
                })
            })
            .collect();
        let messages = provider_messages(history, &[]);
        assert_eq!(messages.len(), 21);
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[1]["content"], "5");
        assert_eq!(messages.last().unwrap()["content"], "24");
    }
}
