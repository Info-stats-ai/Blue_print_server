use std::collections::HashMap;

use serde_json::json;
use similar::{ChangeTag, TextDiff};

use crate::AppState;
use crate::models::ChatMessage;
use crate::ws::{self, Hub};

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

// Six fixed sections, in this exact order and heading text, every time —
// "## Work Split" in particular is parsed back out deterministically by
// `parse_work_split` (see below) to drive the work-lane warning, so the
// model can't be allowed to rename or reorder it.
const BLUEPRINT_SYSTEM_PROMPT_TEMPLATE: &str = "\
From the conversation and the project's current file contents below, produce a structured blueprint.md for this team. \
Use exactly these six '##' sections, in this order, every time, with these exact headings:\n\n\
## Architecture\nA short paragraph on the overall design and how the pieces fit together.\n\n\
## File Structure\nA bullet per synced file, one line each: `path`: its role.\n\n\
## Work Split\nAssign every synced file to exactly one owner from the current participants: {participants}. \
One bullet per owner, in the form `- participant_id: path1, path2, ...`. If a file's owner isn't clear from the \
conversation, assign it to whichever participant most recently touched it, or to `agent` if none has.\n\n\
## Contracts\nEach key module/function as a bullet `- `symbol_name`: one line of intended behavior`. Keep every symbol the team names in backticks.\n\n\
## Roadmap\nUp to 5 bullets: what's next, in priority order.\n\n\
## Guardrails\nUp to 5 bullets: constraints or invariants the team must not violate.\n\n\
Under 600 words total.\n\n\
Current participants: {participants}\n\n\
Current file contents:\n{file_context}";

// Caps each synced file's content in the blueprint prompt so a single large
// file can't blow out the request; this only affects the blueprint-generation
// context block, not the diff the save-watcher checks in check_file_save.
const MAX_FILE_CONTEXT_CHARS: usize = 12_000;

// Caps the blueprint prompt by total content size rather than file count.
// A budget, not a count, so the number of files included varies with how
// many are actually relevant to the conversation and how big they are —
// a session that's been discussing 3 files gets 3, a session discussing 15
// small ones can fit more than a fixed "8" ever would, and it still can't
// grow unboundedly with total headcount or files-ever-synced the way the
// old flat per-file cap didn't prevent.
const BLUEPRINT_CONTEXT_BUDGET_CHARS: usize = 60_000;

// Below this length a word is almost certainly noise (articles, "is", "to",
// coordinate the chat) rather than a symbol or project-specific term worth
// ranking files by, so it's dropped before being used as a query term.
const MIN_QUERY_TERM_LEN: usize = 4;

// Caps the diff text sent to the watcher per save, so a save that replaces
// most of a large file (effectively "no previous version to diff against")
// can't blow out the request the same way a whole-file resend would.
const MAX_DIFF_CHARS: usize = 4_000;

// Bounds the total raw content `ws::Hub::relevant_files` pulls in while
// ranking candidates for cross-file context — generous, since this is just
// scan input, not what's sent to the model (that's `MAX_CROSS_FILE_CONTEXT_CHARS`,
// applied after the relevant lines are extracted below).
pub const CROSS_FILE_SCAN_BUDGET_CHARS: usize = 20_000;
// Caps how many positive matches get kept and the total size of the
// resulting context block — this is what gives the watcher codebase-wide
// awareness (catching that a change affects callers in files nobody has
// open) without the cost of a full project index: bounded by the symbols
// actually in play for this one change, not by project size or headcount.
const MAX_CROSS_FILE_MATCHES: usize = 5;
const MAX_CROSS_FILE_CONTEXT_CHARS: usize = 2_000;

fn model_or_fallback() -> String {
    std::env::var("AGENT_MODEL").unwrap_or_else(|_| FALLBACK_AGENT_MODEL.to_string())
}

/// Handles an "@agent" mention: builds context from recent chat history,
/// calls the Anthropic API, and broadcasts the reply as a chat message from
/// AGENT_USER_ID. Runs as its own tokio task so it never blocks a socket's
/// receive loop; any failure is reported into the chat instead of panicking.
pub async fn handle_mention(state: AppState, session_id: String) {
    let history = state
        .hub
        .recent_chat_history(&session_id, HISTORY_WINDOW)
        .await;
    let messages = build_messages(&history);
    if messages.is_empty() {
        return;
    }

    let blueprint_content = state.hub.get_blueprint(&session_id).await;
    let model = model_or_fallback();
    println!("agent: replying in session {session_id} using model {model}");

    let system = MENTION_SYSTEM_PROMPT_TEMPLATE.replace("{blueprint_content}", &blueprint_content);
    let text = match call_anthropic(&state.http_client, &model, &system, messages).await {
        Ok((text, cost)) => {
            state.hub.record_llm_call(&session_id, cost).await;
            text
        }
        Err(reason) => {
            eprintln!("agent: API error in session {session_id}: {reason}");
            format!("(agent error: {reason})")
        }
    };

    broadcast_agent_reply(&state.hub, &session_id, text).await;
}

/// Single-flight wrapper around `handle_blueprint_request`: with 50 people in
/// a session, "@agent blueprint" firing from several of them in a short
/// window would otherwise spawn that many concurrent regenerations, all
/// racing to overwrite the same shared blueprint. This ensures only one job
/// runs per session at a time; triggers that arrive while it's running are
/// coalesced into exactly one more run afterward, not one each.
pub async fn run_blueprint_job(state: AppState, session_id: String) {
    if !state.hub.try_start_blueprint_job(&session_id).await {
        // A job is already running for this session; the trigger has been
        // recorded and will cause one rerun after the current job finishes.
        return;
    }
    loop {
        handle_blueprint_request(state.clone(), session_id.clone()).await;
        if !state.hub.finish_blueprint_job(&session_id).await {
            break;
        }
        // Another trigger arrived mid-run; re-claim the slot and run once
        // more with whatever's current now, then re-check again on finish.
        if !state.hub.try_start_blueprint_job(&session_id).await {
            break;
        }
    }
}

/// Handles an explicit "@agent blueprint" request: summarizes the recent
/// planning conversation into a blueprint.md, stores it as the session's
/// blueprint, broadcasts the update, and posts a short confirmation to chat.
/// One LLM call; any failure is reported into chat instead of panicking.
pub async fn handle_blueprint_request(state: AppState, session_id: String) {
    let history = state
        .hub
        .recent_chat_history(&session_id, BLUEPRINT_HISTORY_WINDOW)
        .await;
    let messages = build_messages(&history);
    if messages.is_empty() {
        return;
    }

    let query_terms = extract_query_terms(&history);
    let files = state
        .hub
        .relevant_files(
            &session_id,
            &query_terms,
            None,
            BLUEPRINT_CONTEXT_BUDGET_CHARS,
        )
        .await;
    let file_context = format_file_context(&files);
    let mut participants = state.hub.peer_ids(&session_id).await;
    participants.push(ws::AGENT_USER_ID.to_string());
    let participant_list = participants.join(", ");
    let model = model_or_fallback();
    println!("agent: generating blueprint in session {session_id} using model {model}");

    let system = BLUEPRINT_SYSTEM_PROMPT_TEMPLATE
        .replace("{participants}", &participant_list)
        .replace("{file_context}", &file_context);
    match call_anthropic(&state.http_client, &model, &system, messages).await {
        Ok((content, cost)) => {
            state.hub.record_llm_call(&session_id, cost).await;
            state.hub.set_blueprint(&session_id, content.clone()).await;
            state
                .hub
                .broadcast(
                    &session_id,
                    &json!({"type": "blueprint_update", "content": content}),
                )
                .await;
            broadcast_agent_reply(&state.hub, &session_id, "Reference updated.".to_string()).await;
        }
        Err(reason) => {
            eprintln!("agent: blueprint generation API error in session {session_id}: {reason}");
            broadcast_agent_reply(&state.hub, &session_id, format!("(agent error: {reason})"))
                .await;
        }
    }
}

/// Formats the files the caller selected via `Hub::relevant_files` (already
/// bounded to `BLUEPRINT_CONTEXT_BUDGET_CHARS` in aggregate) as a context
/// block for the blueprint prompt, capping each individual file's content so
/// one large file can't dominate the budget on its own.
fn format_file_context(files: &[(String, String)]) -> String {
    if files.is_empty() {
        return "(no files synced yet)".to_string();
    }
    files
        .iter()
        .map(|(path, content)| {
            format!(
                "File: {path}\n{}",
                truncate_chars(content, MAX_FILE_CONTEXT_CHARS)
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Pulls candidate symbol/term names out of the planning conversation to
/// rank synced files by relevance against — e.g. if the team's been
/// discussing `calculate_total`, that word becomes a query term and any
/// file whose structural index (see `symbols.rs`) defines a symbol by that
/// name ranks above files that don't, regardless of which was synced most
/// recently. Deliberately simple (whitespace/punctuation split, a length
/// floor, no stopword list beyond that): false positives here just add a
/// harmless low-weight text-match candidate, so there's little downside to
/// erring toward keeping a word.
fn extract_query_terms(history: &[ChatMessage]) -> Vec<String> {
    let mut terms = Vec::new();
    for entry in history {
        for word in entry.text.split(|c: char| !c.is_alphanumeric() && c != '_') {
            if word.chars().count() < MIN_QUERY_TERM_LEN {
                continue;
            }
            let word = word.to_string();
            if !terms.contains(&word) {
                terms.push(word);
            }
        }
    }
    terms
}

/// Produces a compact unified-style diff between the previously synced
/// version of a file and the new one, so the watcher only ever sees what
/// actually changed instead of the whole file on every save. An empty
/// `previous` (first sync of this path) just yields the new content, capped
/// the same way.
pub fn diff_content(previous: &str, next: &str) -> String {
    if previous.is_empty() {
        return truncate_chars(next, MAX_DIFF_CHARS);
    }
    let diff = TextDiff::from_lines(previous, next);
    let mut rendered = String::new();
    for change in diff.iter_all_changes() {
        let marker = match change.tag() {
            ChangeTag::Delete => "-",
            ChangeTag::Insert => "+",
            ChangeTag::Equal => continue,
        };
        rendered.push_str(marker);
        rendered.push_str(change.as_str().unwrap_or_default());
        if !rendered.ends_with('\n') {
            rendered.push('\n');
        }
    }
    if rendered.is_empty() {
        // Lines are identical but something else in the file changed
        // (whitespace-only, etc.) — fall back to a marker so the watcher
        // still has something to reason about instead of an empty diff.
        rendered.push_str("(no line-level changes detected)");
    }
    truncate_chars(&rendered, MAX_DIFF_CHARS)
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

/// Parses the blueprint's "## Work Split" section (the exact heading the
/// template in `BLUEPRINT_SYSTEM_PROMPT_TEMPLATE` requires) into path ->
/// owner. Drives the Phase 6 work-lane warning: advisory only, so a miss
/// here just means one fewer warning fires, never a false one — matching
/// the bullet format `- owner_id: path1, path2, ...` is a best effort, not
/// a hard parse the model is guaranteed to satisfy every time.
pub fn parse_work_split(blueprint: &str) -> HashMap<String, String> {
    let mut owners = HashMap::new();
    let Some(section_start) = blueprint.find("## Work Split") else {
        return owners;
    };
    let section = &blueprint[section_start + "## Work Split".len()..];
    let section_body = section.split("\n## ").next().unwrap_or(section);
    for line in section_body.lines() {
        let line = line.trim().trim_start_matches('-').trim();
        let Some((owner, paths)) = line.split_once(':') else {
            continue;
        };
        let owner = owner.trim().trim_matches('`');
        if owner.is_empty() {
            continue;
        }
        for path in paths.split(',') {
            let path = path.trim().trim_matches('`');
            if !path.is_empty() {
                owners.insert(path.to_string(), owner.to_string());
            }
        }
    }
    owners
}

/// Formats the symbol-relevant lines out of `other_files` — already ranked
/// and selected by `ws::Hub::relevant_files` against the change's matched
/// symbols — into a short block naming where each one shows up. This is the
/// mechanism that gives the watcher codebase-wide awareness: a change to
/// `calculate_total` that `invoice.rs` silently relies on gets flagged even
/// though nobody has `invoice.rs` open right now, and because the ranking
/// comes from the structural symbol index rather than "recently synced,"
/// it finds `invoice.rs` even if it hasn't been touched this session.
pub fn cross_file_context(other_files: &[(String, String)], matched_symbols: &[String]) -> String {
    let mut matches = Vec::new();
    for (path, content) in other_files {
        if matches.len() >= MAX_CROSS_FILE_MATCHES {
            break;
        }
        if let Some(line) = matched_symbols
            .iter()
            .find_map(|symbol| content.lines().find(|line| line.contains(symbol.as_str())))
        {
            matches.push(format!("{path}: {}", line.trim()));
        }
    }
    if matches.is_empty() {
        return "(no other synced files reference these symbols)".to_string();
    }
    truncate_chars(&matches.join("\n"), MAX_CROSS_FILE_CONTEXT_CHARS)
}

/// Runs the single paid step of the watcher: one Haiku call asking whether a
/// saved file's change stays consistent with the blueprint for the symbols
/// the free check already found present. Only called when
/// `matching_blueprint_symbols` returned a non-empty list, and only after the
/// caller's debounce window — `diff` is the change itself (see
/// `diff_content`), not the whole file, so cost stays proportional to what
/// actually changed. Stays silent on any API error (logged, not broadcast)
/// so a flaky call can't spam chat; replies of exactly "OK" are also dropped
/// so the team only hears about deviations.
pub async fn check_file_save(
    state: AppState,
    session_id: String,
    path: String,
    diff: String,
    cross_file: String,
    blueprint: String,
    matched_symbols: Vec<String>,
) {
    let model = model_or_fallback();
    println!("agent: watcher checking {path} in session {session_id} using model {model}");

    let symbol_list = matched_symbols.join(", ");
    let system = format!(
        "You are watching code changes against a shared team blueprint. Blueprint:\n{blueprint}\n\nDoes this change stay consistent with the blueprint's intent for these symbols: {symbol_list}? Also consider whether it breaks an assumption other files in the session make about these symbols. Reply in ONE short sentence. If it deviates, or breaks something another file relies on, say so and name it. If it's fine, reply exactly: OK.",
    );
    let messages = vec![json!({"role": "user", "content": format!(
        "File: {path}\n\nDiff of the change (+ added, - removed):\n{diff}\n\nOther synced files referencing these symbols:\n{cross_file}"
    )})];

    let reply = match call_anthropic(&state.http_client, &model, &system, messages).await {
        Ok((reply, cost)) => {
            state.hub.record_llm_call(&session_id, cost).await;
            reply
        }
        Err(reason) => {
            eprintln!("agent: watcher API error in session {session_id} for {path}: {reason}");
            return;
        }
    };

    if reply.trim().eq_ignore_ascii_case("ok") {
        return;
    }
    // Anything other than a bare "OK" means the watcher flagged a genuine
    // deviation — this is a divergence the agent caught before it became a
    // silent merge conflict, so it counts toward the "divergences caught"
    // half of Phase 6's conflict-prevention metric.
    state.hub.record_divergence_caught(&session_id).await;

    broadcast_agent_reply(&state.hub, &session_id, reply).await;
}

async fn broadcast_agent_reply(hub: &Hub, session_id: &str, text: String) {
    let message = ChatMessage {
        sender: ws::AGENT_USER_ID.to_string(),
        text,
    };
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
        let role = if entry.sender == ws::AGENT_USER_ID {
            "assistant"
        } else {
            "user"
        };
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

    // A trailing assistant turn means nothing new has been said since the
    // agent's own last reply — most visibly, the single-flight rerun in
    // `run_blueprint_job`: by the time it re-reads history, all it finds
    // past the request that triggered it is the agent's own prior
    // "Blueprint updated." Sending that as the last message is a dangling
    // prefill, which current-generation models reject with a 400. Trimming
    // it leaves nothing to summarize, which the caller's `is_empty` check
    // already treats as a no-op — correct, since there genuinely isn't
    // anything new to react to.
    while matches!(turns.last(), Some((role, _)) if *role == "assistant") {
        turns.pop();
    }

    turns
        .into_iter()
        .map(|(role, content)| json!({"role": role, "content": content}))
        .collect()
}

/// Rough per-model USD-per-million-token rates (input, output). Deliberately
/// approximate — see `estimate_cost_usd` — this feeds the live "API cost per
/// session" metric (Phase 6's differentiator overlay), not billing, so a
/// ballpark that's right within the same order of magnitude is the bar, not
/// cent-exact reconciliation against the real invoice.
fn model_rates_per_million(model: &str) -> (f64, f64) {
    if model.contains("sonnet") {
        (2.00, 10.00)
    } else if model.contains("opus") {
        (4.00, 20.00)
    } else {
        // Haiku, and anything unrecognized: this project's actual fallback
        // model is Haiku, so defaulting here undercounts an unrecognized
        // *expensive* model rather than overcounting a cheap one — the
        // safer direction for a cost estimate nobody should over-trust.
        (0.10, 0.50)
    }
}

fn estimate_cost_usd(model: &str, input_tokens: u64, output_tokens: u64) -> f64 {
    let (input_rate, output_rate) = model_rates_per_million(model);
    (input_tokens as f64 / 1_000_000.0) * input_rate
        + (output_tokens as f64 / 1_000_000.0) * output_rate
}

/// Returns the reply text plus an estimated USD cost for the call (see
/// `estimate_cost_usd`), so every caller can feed the session's running
/// cost metric without duplicating the `usage` parsing.
async fn call_anthropic(
    client: &reqwest::Client,
    model: &str,
    system: &str,
    messages: Vec<serde_json::Value>,
) -> Result<(String, f64), String> {
    let api_key = std::env::var("ANTHROPIC_API_KEY")
        .map_err(|_| "ANTHROPIC_API_KEY is not set".to_string())?;

    let body = json!({
        "model": model,
        "max_tokens": 4096,
        "system": system,
        "messages": messages,
    });

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
    let raw_body = response
        .text()
        .await
        .map_err(|error| format!("failed to read response body: {error}"))?;

    if !status.is_success() {
        return Err(format!("API returned {status}"));
    }

    let value: serde_json::Value = serde_json::from_str(&raw_body)
        .map_err(|error| format!("invalid response body: {error}"))?;

    // The response's `content` array can include non-text blocks (e.g. `thinking`)
    // ahead of the text block, so find the first block whose `type` is `text`
    // rather than assuming `content[0]` is it.
    let text = value
        .get("content")
        .and_then(|content| content.as_array())
        .and_then(|blocks| {
            blocks
                .iter()
                .find(|block| block.get("type").and_then(|t| t.as_str()) == Some("text"))
        })
        .and_then(|block| block.get("text"))
        .and_then(|text| text.as_str())
        .map(str::to_string)
        .ok_or_else(|| "unexpected response shape".to_string())?;

    let input_tokens = value
        .get("usage")
        .and_then(|u| u.get("input_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let output_tokens = value
        .get("usage")
        .and_then(|u| u.get("output_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let cost = estimate_cost_usd(model, input_tokens, output_tokens);

    Ok((text, cost))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_work_split_bullets() {
        let blueprint = "## Architecture\nSome text.\n\n## Work Split\n- alice: `src/auth.rs`, `src/login.rs`\n- bob: `src/billing.rs`\n\n## Contracts\n- `calculate_total`: sums line items.\n";
        let owners = parse_work_split(blueprint);
        assert_eq!(owners.get("src/auth.rs"), Some(&"alice".to_string()));
        assert_eq!(owners.get("src/login.rs"), Some(&"alice".to_string()));
        assert_eq!(owners.get("src/billing.rs"), Some(&"bob".to_string()));
        assert_eq!(owners.len(), 3);
    }

    #[test]
    fn missing_work_split_section_returns_empty() {
        assert_eq!(
            parse_work_split("## Architecture\nNo split here.\n"),
            HashMap::new()
        );
    }

    #[test]
    fn cost_estimate_scales_with_tokens_and_model() {
        let haiku = estimate_cost_usd("claude-haiku-5-5", 1_000_000, 1_000_000);
        assert!((haiku - 0.60).abs() < 1e-9);

        let sonnet = estimate_cost_usd("claude-sonnet-5-5", 1_000_000, 1_000_000);
        assert!((sonnet - 12.00).abs() < 1e-9);
        assert!(sonnet > haiku);

        assert_eq!(estimate_cost_usd("claude-haiku-5-5", 0, 0), 0.0);
    }

    #[test]
    fn extract_query_terms_drops_short_words_and_dedupes() {
        let history = vec![
            ChatMessage {
                sender: "alice".to_string(),
                text: "let's fix calculate_total in invoice.rs".to_string(),
            },
            ChatMessage {
                sender: "bob".to_string(),
                text: "ok, calculate_total needs tax now".to_string(),
            },
        ];
        let terms = extract_query_terms(&history);
        assert!(terms.contains(&"calculate_total".to_string()));
        assert!(terms.contains(&"invoice".to_string()));
        assert!(!terms.contains(&"ok".to_string())); // 2 chars, below MIN_QUERY_TERM_LEN
        let occurrences = terms.iter().filter(|t| *t == "calculate_total").count();
        assert_eq!(occurrences, 1, "query terms must be deduplicated");
    }
}
