use std::collections::HashMap;

use serde_json::json;

use crate::models::ChatMessage;
use crate::ws::{self, Hub};
use crate::AppState;

// The model to use when the AGENT_MODEL env var isn't set. Keep this in sync
// with whatever the current Haiku model id is; Railway should normally
// override it via AGENT_MODEL.
const FALLBACK_AGENT_MODEL: &str = "claude-haiku-5-5";

const ANTHROPIC_API_URL: &str = "https://api.anthropic.com/v1/messages";
const ANTHROPIC_VERSION: &str = "2023-06-01";

// How many recent chat messages (across all senders) to send as context for
// a normal reply vs. a blueprint-generation request (which wants more of the
// planning conversation).
const HISTORY_WINDOW: usize = 10;
const BLUEPRINT_HISTORY_WINDOW: usize = 15;

const MENTION_SYSTEM_PROMPT_TEMPLATE: &str = "You are a coding assistant embedded in a shared IDE session helping two developers coordinate. Keep replies short and actionable. Shared blueprint reference file:\n{blueprint_content}";

const BLUEPRINT_SYSTEM_PROMPT_TEMPLATE: &str = "From the conversation and the project's current file contents below, produce a concise blueprint.md. Format: a '## Contracts' section listing each key module/function as a bullet `- `symbol_name`: one line of intended behavior`. Keep every symbol the team names in backticks. Under 400 words.\n\nCurrent file contents:\n{file_context}";

// Caps each synced file's content in the blueprint prompt so a single large
// file can't blow out the request; this only affects the blueprint-generation
// context block, not the full content the save-watcher checks in check_file_save.
const MAX_FILE_CONTEXT_CHARS: usize = 12_000;

fn model_or_fallback() -> String {
    std::env::var("AGENT_MODEL").unwrap_or_else(|_| FALLBACK_AGENT_MODEL.to_string())
}

/// Handles an "@agent" mention: builds context from recent chat history,
/// calls the Anthropic API, and broadcasts the reply as a chat message from
/// AGENT_USER_ID. Runs as its own tokio task so it never blocks a socket's
/// receive loop; any failure is reported into the chat instead of panicking.
pub async fn handle_mention(state: AppState, session_id: String) {
    let history = state.hub.recent_chat_history(&session_id, HISTORY_WINDOW).await;
    let messages = build_messages(&history);
    if messages.is_empty() {
        return;
    }

    let blueprint_content = state.hub.get_blueprint(&session_id).await;
    let model = model_or_fallback();
    println!("agent: replying in session {session_id} using model {model}");

    let system = MENTION_SYSTEM_PROMPT_TEMPLATE.replace("{blueprint_content}", &blueprint_content);
    let text = match call_anthropic(&model, &system, messages).await {
        Ok(text) => text,
        Err(reason) => {
            eprintln!("agent: API error in session {session_id}: {reason}");
            format!("(agent error: {reason})")
        }
    };

    broadcast_agent_reply(&state.hub, &session_id, text).await;
}

/// Handles an explicit "@agent blueprint" request: summarizes the recent
/// planning conversation into a blueprint.md, stores it as the session's
/// blueprint, broadcasts the update, and posts a short confirmation to chat.
/// One LLM call; any failure is reported into chat instead of panicking.
pub async fn handle_blueprint_request(state: AppState, session_id: String) {
    let history = state.hub.recent_chat_history(&session_id, BLUEPRINT_HISTORY_WINDOW).await;
    let messages = build_messages(&history);
    if messages.is_empty() {
        return;
    }

    let files = state.hub.get_files(&session_id).await;
    let file_context = format_file_context(&files);
    let model = model_or_fallback();
    println!("agent: generating blueprint in session {session_id} using model {model}");

    let system = BLUEPRINT_SYSTEM_PROMPT_TEMPLATE.replace("{file_context}", &file_context);
    match call_anthropic(&model, &system, messages).await {
        Ok(content) => {
            state.hub.set_blueprint(&session_id, content.clone()).await;
            state
                .hub
                .broadcast(&session_id, &json!({"type": "blueprint_update", "content": content}))
                .await;
            broadcast_agent_reply(&state.hub, &session_id, "Blueprint updated.".to_string()).await;
        }
        Err(reason) => {
            eprintln!("agent: blueprint generation API error in session {session_id}: {reason}");
            broadcast_agent_reply(&state.hub, &session_id, format!("(agent error: {reason})")).await;
        }
    }
}

/// Formats synced files as a context block for the blueprint prompt, oldest
/// path order aside (HashMap has none), capping each file's content so one
/// large file can't dominate the request budget.
fn format_file_context(files: &HashMap<String, String>) -> String {
    if files.is_empty() {
        return "(no files synced yet)".to_string();
    }
    files
        .iter()
        .map(|(path, content)| format!("File: {path}\n{}", truncate_chars(content, MAX_FILE_CONTEXT_CHARS)))
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Truncates by character count, not byte count, so a cut can't land inside a
/// multi-byte UTF-8 sequence and panic.
fn truncate_chars(content: &str, max_chars: usize) -> String {
    if content.chars().count() <= max_chars {
        return content.to_string();
    }
    let mut truncated: String = content.chars().take(max_chars).collect();
    truncated.push_str("\n... (truncated)");
    truncated
}

/// Extracts every backticked symbol from the blueprint and returns the
/// subset that literally appears in a saved file's content. This is the free
/// check: it runs on every file_save with no LLM call, and an empty result
/// means the watcher does nothing (the common path, by design for cost
/// control). Assumes backticks in the blueprint are balanced; an odd count
/// just drops the final, unterminated span, which is harmless here since
/// this is a heuristic prefilter, not a correctness-critical parse.
pub fn matching_blueprint_symbols(blueprint: &str, content: &str) -> Vec<String> {
    blueprint
        .split('`')
        .skip(1)
        .step_by(2)
        .filter(|symbol| !symbol.is_empty())
        .filter(|symbol| content.contains(symbol))
        .map(str::to_string)
        .collect()
}

/// Runs the single paid step of the watcher: one Haiku call asking whether a
/// saved file stays consistent with the blueprint for the symbols the free
/// check already found present. Only called when `matching_blueprint_symbols`
/// returned a non-empty list. Stays silent on any API error (logged, not
/// broadcast) so a flaky call can't spam chat; replies of exactly "OK" are
/// also dropped so the team only hears about deviations.
pub async fn check_file_save(
    state: AppState,
    session_id: String,
    path: String,
    content: String,
    blueprint: String,
    matched_symbols: Vec<String>,
) {
    let model = model_or_fallback();
    println!("agent: watcher checking {path} in session {session_id} using model {model}");

    let symbol_list = matched_symbols.join(", ");
    let system = format!(
        "You are watching code changes against a shared team blueprint. Blueprint:\n{blueprint}\n\nDoes this change stay consistent with the blueprint's intent for these symbols: {symbol_list}? Reply in ONE short sentence. If it deviates or adds something the blueprint doesn't call for, say so and name it. If it's fine, reply exactly: OK.",
    );
    let messages = vec![json!({"role": "user", "content": format!("File: {path}\n\n{content}")})];

    let reply = match call_anthropic(&model, &system, messages).await {
        Ok(reply) => reply,
        Err(reason) => {
            eprintln!("agent: watcher API error in session {session_id} for {path}: {reason}");
            return;
        }
    };

    if reply.trim().eq_ignore_ascii_case("ok") {
        return;
    }

    broadcast_agent_reply(&state.hub, &session_id, reply).await;
}

async fn broadcast_agent_reply(hub: &Hub, session_id: &str, text: String) {
    let message = ChatMessage { sender: ws::AGENT_USER_ID.to_string(), text };
    hub.push_chat_message(session_id, message.clone()).await;
    hub.broadcast(
        session_id,
        &json!({"type": "chat", "sender": message.sender, "text": message.text}),
    )
    .await;
}

/// Maps history into Anthropic's alternating user/assistant turns, prefixing
/// each user line with its sender so the agent can tell developers apart
/// even though they all map to the same "user" role. Consecutive same-role
/// entries are concatenated since the API rejects back-to-back same-role turns.
fn build_messages(history: &[ChatMessage]) -> Vec<serde_json::Value> {
    let mut turns: Vec<(&'static str, String)> = Vec::new();
    for entry in history {
        let role = if entry.sender == ws::AGENT_USER_ID { "assistant" } else { "user" };
        let line = if role == "user" {
            format!("[{}] {}", entry.sender, entry.text)
        } else {
            entry.text.clone()
        };
        match turns.last_mut() {
            Some((last_role, content)) if *last_role == role => {
                content.push('\n');
                content.push_str(&line);
            }
            _ => turns.push((role, line)),
        }
    }

    // Anthropic requires the first turn to come from the user.
    while matches!(turns.first(), Some((role, _)) if *role == "assistant") {
        turns.remove(0);
    }

    turns
        .into_iter()
        .map(|(role, content)| json!({"role": role, "content": content}))
        .collect()
}

async fn call_anthropic(
    model: &str,
    system: &str,
    messages: Vec<serde_json::Value>,
) -> Result<String, String> {
    let api_key = std::env::var("ANTHROPIC_API_KEY")
        .map_err(|_| "ANTHROPIC_API_KEY is not set".to_string())?;

    let body = json!({
        "model": model,
        "max_tokens": 512,
        "system": system,
        "messages": messages,
    });

    let client = reqwest::Client::new();
    let response = client
        .post(ANTHROPIC_API_URL)
        .header("x-api-key", api_key)
        .header("anthropic-version", ANTHROPIC_VERSION)
        .header("content-type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|error| format!("request failed: {error}"))?;

    let status = response.status();
    if !status.is_success() {
        return Err(format!("API returned {status}"));
    }

    let value: serde_json::Value = response
        .json()
        .await
        .map_err(|error| format!("invalid response body: {error}"))?;

    value
        .get("content")
        .and_then(|content| content.get(0))
        .and_then(|block| block.get("text"))
        .and_then(|text| text.as_str())
        .map(str::to_string)
        .ok_or_else(|| "unexpected response shape".to_string())
}
