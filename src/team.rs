//! The team layer on top of chat: the observer's rolling digest of the
//! discussion, and the task board the planner builds from it.
//!
//! Cost model: the digest is the one place a long discussion gets read by a
//! model, by one cheap call per batch of new messages for the whole session.
//! Everything else that needs "what has the team discussed" (the planner,
//! every member's own agent) reads the digest plus a short recent window, so
//! context cost stays flat however long the thread gets.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::AppState;
use crate::agent;
use crate::models::ChatMessage;
use crate::ws;

/// New messages that accumulate before the observer folds them into the
/// digest. Agents always see the most recent messages verbatim, so the
/// digest only has to keep up with older discussion.
const DIGEST_BATCH_MESSAGES: i64 = 25;
const DIGEST_MAX_NEW_MESSAGES: i64 = 300;
const DIGEST_MAX_MESSAGE_CHARS: usize = 1_500;
const PLANNER_RECENT_MESSAGES: i64 = 60;
const PLANNER_MAX_FILES: usize = 300;
const PLANNER_MAX_BLUEPRINT_CHARS: usize = 6_000;
const PLANNER_ATTEMPTS: usize = 3;

pub const TASK_STATUSES: [&str; 3] = ["todo", "in_progress", "done"];

const DIGEST_SYSTEM_PROMPT: &str = "\
You maintain the running memory of a software team's discussion. You get the current summary and the messages that came after it. \
Return an updated summary that fully replaces the old one. Keep it under 400 words, in Markdown, with exactly these sections:\n\
## Goal\n## Decisions\n## People and roles\n## Open questions\n## Current focus\n\
Under 'People and roles', list every participant by name with the role, skills, or work they claimed or were given. \
Keep decisions and commitments (who said they would do what); drop small talk. Never invent facts.";

const PLANNER_SYSTEM_PROMPT: &str = "\
You are the planner for a software team that works with AI coding agents. Turn the team's discussion into tasks, one owner each.\n\
Rules:\n\
- Infer each person's role from what they said (\"I'll take the frontend\", \"I know Postgres\"). Fit tasks to roles and spread the work across everyone listed.\n\
- Make tasks that can run in parallel. Give different people different files wherever possible, so no two people or agents edit the same file.\n\
- Each task must be concrete enough for an AI agent to carry out alone: what to build, which files, and how to tell it is done.\n\
- Use the exact user_id values from the team list for assignee.\n\
Reply with JSON only, no prose, in this shape:\n\
{\"summary\": \"one or two sentences on the plan\", \"tasks\": [{\"title\": \"...\", \"detail\": \"...\", \"assignee\": \"<user_id>\", \"files\": [\"path\"], \"depends_on\": [1]}]}\n\
depends_on lists the 1-based numbers of tasks that must finish first.";

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TaskBoard {
    pub summary: String,
    pub tasks: Vec<Task>,
    // Bumped on every regeneration, so task ids from different plans never
    // collide.
    pub version: u64,
    pub generated_by: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Task {
    pub id: String,
    pub title: String,
    pub detail: String,
    pub assignee: Option<String>,
    pub files: Vec<String>,
    pub depends_on: Vec<String>,
    pub status: String,
    pub updated_by: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentActivity {
    pub status: String,
    pub detail: Option<String>,
    pub task_id: Option<String>,
}

impl TaskBoard {
    /// Path -> assignee for unfinished tasks, for the work-lane warning.
    pub fn file_owners(&self) -> HashMap<String, String> {
        self.tasks
            .iter()
            .filter(|task| task.status != "done")
            .filter_map(|task| Some((task.assignee.clone()?, task)))
            .flat_map(|(assignee, task)| {
                task.files
                    .iter()
                    .map(move |path| (path.clone(), assignee.clone()))
            })
            .collect()
    }
}

#[derive(Deserialize)]
struct RawPlan {
    #[serde(default)]
    summary: String,
    #[serde(default)]
    tasks: Vec<RawTask>,
}

#[derive(Deserialize)]
struct RawTask {
    title: String,
    #[serde(default, alias = "description")]
    detail: String,
    #[serde(default, alias = "owner")]
    assignee: Option<String>,
    #[serde(default)]
    files: Vec<String>,
    #[serde(default)]
    depends_on: Vec<serde_json::Value>,
}

/// Loads the digest and task board from the database into a channel that
/// doesn't have them yet (a fresh process, or an evicted session).
pub async fn rehydrate(state: &AppState, session_id: &str) {
    let (digest, through_id) = state.hub.get_digest(session_id).await;
    if digest.is_empty() && through_id == 0 {
        let row: Option<(String, i64)> =
            sqlx::query_as("SELECT content, through_id FROM session_digests WHERE session_id = ?")
                .bind(session_id)
                .fetch_optional(&state.pool)
                .await
                .unwrap_or_else(|error| {
                    eprintln!("team: loading digest for {session_id} failed: {error}");
                    None
                });
        if let Some((content, through_id)) = row {
            state.hub.set_digest(session_id, content, through_id).await;
        }
    }
    if state.hub.get_tasks(session_id).await.tasks.is_empty() {
        let row: Option<String> =
            sqlx::query_scalar("SELECT board FROM session_tasks WHERE session_id = ?")
                .bind(session_id)
                .fetch_optional(&state.pool)
                .await
                .unwrap_or_else(|error| {
                    eprintln!("team: loading tasks for {session_id} failed: {error}");
                    None
                });
        if let Some(board) = row {
            match serde_json::from_str::<TaskBoard>(&board) {
                Ok(board) => state.hub.set_tasks(session_id, board).await,
                Err(error) => eprintln!("team: stored task board for {session_id} is invalid: {error}"),
            }
        }
    }
}

/// Starts a digest pass once enough new messages have piled up. Called
/// after every stored chat message; cheap when there's nothing to do.
pub async fn maybe_refresh_digest(state: &AppState, session_id: &str, latest_id: i64) {
    let (_, through_id) = state.hub.get_digest(session_id).await;
    if latest_id - through_id < DIGEST_BATCH_MESSAGES {
        return;
    }
    let state = state.clone();
    let session_id = session_id.to_string();
    tokio::spawn(async move {
        run_digest_job(state, session_id).await;
    });
}

/// Single-flight, like the blueprint job: concurrent triggers collapse into
/// one more pass after the running one.
async fn run_digest_job(state: AppState, session_id: String) {
    if !state.hub.try_start_job(&session_id, "digest").await {
        return;
    }
    loop {
        if let Err(error) = refresh_digest(&state, &session_id).await {
            eprintln!("team: digest failed in session {session_id}: {error}");
        }
        if !state.hub.finish_job(&session_id, "digest").await
            || !state.hub.try_start_job(&session_id, "digest").await
        {
            break;
        }
    }
}

/// Folds every message after the current digest into it, in bounded
/// batches. Callers that need an up-to-date digest (the planner) await it.
pub async fn refresh_digest(state: &AppState, session_id: &str) -> Result<(), String> {
    let names = ws::member_display_names(state, session_id).await;
    loop {
        let (digest, through_id) = state.hub.get_digest(session_id).await;
        let messages: Vec<ChatMessage> = sqlx::query_as(
            "SELECT id, sender, text, author FROM chat_messages \
             WHERE session_id = ? AND id > ? ORDER BY id LIMIT ?",
        )
        .bind(session_id)
        .bind(through_id)
        .bind(DIGEST_MAX_NEW_MESSAGES)
        .fetch_all(&state.pool)
        .await
        .map_err(|error| error.to_string())?;
        let Some(last) = messages.last() else {
            return Ok(());
        };
        let new_through_id = last.id;
        let batch_was_full = messages.len() as i64 == DIGEST_MAX_NEW_MESSAGES;

        let transcript = format_transcript(&messages, &names, DIGEST_MAX_MESSAGE_CHARS);
        let current = if digest.is_empty() { "(none yet)" } else { digest.as_str() };
        let model = std::env::var("AGENT_DIGEST_MODEL").unwrap_or_else(|_| agent::model_or_fallback());
        let request = vec![json!({"role": "user", "content": format!(
            "Current summary:\n{current}\n\nNew messages:\n{transcript}"
        )})];
        let (content, cost) =
            agent::call_model(&state.http_client, &model, DIGEST_SYSTEM_PROMPT, request).await?;
        state.hub.record_llm_call(session_id, cost).await;
        let content = content.trim().to_string();
        if content.is_empty() {
            return Err("the model returned an empty summary".to_string());
        }
        if state
            .hub
            .set_digest(session_id, content.clone(), new_through_id)
            .await
        {
            sqlx::query(
                "INSERT INTO session_digests (session_id, content, through_id) VALUES (?, ?, ?) \
                 ON CONFLICT(session_id) DO UPDATE SET content = excluded.content, \
                 through_id = excluded.through_id, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')",
            )
            .bind(session_id)
            .bind(&content)
            .bind(new_through_id)
            .execute(&state.pool)
            .await
            .map_err(|error| error.to_string())?;
            state
                .hub
                .broadcast(
                    session_id,
                    &json!({"type": "digest_update", "content": content, "through_id": new_through_id}),
                )
                .await;
        }
        if !batch_was_full {
            return Ok(());
        }
    }
}

fn format_transcript(messages: &[ChatMessage], names: &HashMap<String, String>, max_chars: usize) -> String {
    let name_of = |user_id: &str| names.get(user_id).cloned().unwrap_or_else(|| user_id.to_string());
    messages
        .iter()
        .map(|message| {
            let speaker = match &message.author {
                Some(author) => format!("{} for {}", message.sender, name_of(author)),
                None if message.sender == ws::AGENT_USER_ID => "agent".to_string(),
                None => name_of(&message.sender),
            };
            let mut text: String = message.text.chars().take(max_chars).collect();
            if text.len() < message.text.len() {
                text.push('…');
            }
            format!("[{speaker}] {text}")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

async fn session_leader(state: &AppState, session_id: &str) -> Option<(bool, String)> {
    sqlx::query_as("SELECT has_leader, owner_id FROM sessions WHERE id = ?")
        .bind(session_id)
        .fetch_optional(&state.pool)
        .await
        .unwrap_or_else(|error| {
            eprintln!("team: loading leader for {session_id} failed: {error}");
            None
        })
}

/// Whether `user_id` may act for the whole team: anyone in a session
/// without a leader, otherwise only the leader.
async fn may_lead(state: &AppState, session_id: &str, user_id: &str) -> bool {
    match session_leader(state, session_id).await {
        Some((true, owner_id)) => owner_id == user_id,
        Some((false, _)) => true,
        None => false,
    }
}

async fn notify(state: &AppState, session_id: &str, text: &str) {
    state
        .hub
        .broadcast(
            session_id,
            &json!({"type": "chat", "sender": ws::AGENT_USER_ID, "text": text}),
        )
        .await;
}

pub async fn request_assignment(state: &AppState, session_id: &str, user_id: &str) {
    if !may_lead(state, session_id, user_id).await {
        notify(state, session_id, "Only the session leader can assign tasks.").await;
        return;
    }
    if !state.hub.try_start_job(session_id, "assign").await {
        // Already planning; the running job picks up this request's
        // discussion too, so one more pass afterward covers it.
        return;
    }
    state
        .hub
        .broadcast(
            session_id,
            &json!({"type": "task_planning", "active": true, "requested_by": user_id}),
        )
        .await;
    let state = state.clone();
    let session_id = session_id.to_string();
    let user_id = user_id.to_string();
    tokio::spawn(async move {
        loop {
            if let Err(error) = assign_tasks(&state, &session_id, &user_id).await {
                eprintln!("team: assignment failed in session {session_id}: {error}");
                agent::broadcast_agent_reply(
                    &state,
                    &session_id,
                    format!("(couldn't build a task plan: {error})"),
                )
                .await;
            }
            if !state.hub.finish_job(&session_id, "assign").await
                || !state.hub.try_start_job(&session_id, "assign").await
            {
                break;
            }
        }
        state
            .hub
            .broadcast(&session_id, &json!({"type": "task_planning", "active": false}))
            .await;
    });
}

async fn assign_tasks(state: &AppState, session_id: &str, requested_by: &str) -> Result<(), String> {
    if let Err(error) = refresh_digest(state, session_id).await {
        // The recent messages still go to the planner verbatim, so a stale
        // digest degrades the plan rather than blocking it.
        eprintln!("team: digest refresh before planning failed in {session_id}: {error}");
    }
    let (digest, _) = state.hub.get_digest(session_id).await;
    let mut recent: Vec<ChatMessage> = sqlx::query_as(
        "SELECT id, sender, text, author FROM chat_messages WHERE session_id = ? ORDER BY id DESC LIMIT ?",
    )
    .bind(session_id)
    .bind(PLANNER_RECENT_MESSAGES)
    .fetch_all(&state.pool)
    .await
    .map_err(|error| error.to_string())?;
    recent.reverse();
    if recent.is_empty() && digest.is_empty() {
        return Err("there's no discussion to plan from yet".to_string());
    }

    let names = ws::member_display_names(state, session_id).await;
    let online: std::collections::HashSet<String> =
        state.hub.peer_ids(session_id).await.into_iter().collect();
    let mut roster: Vec<(&String, &String)> = names.iter().collect();
    roster.sort();
    let team = roster
        .iter()
        .map(|(user_id, name)| {
            let status = if online.contains(*user_id) { "online" } else { "offline" };
            format!("- user_id: {user_id} | name: {name} | {status}")
        })
        .collect::<Vec<_>>()
        .join("\n");

    let goal: String = sqlx::query_scalar("SELECT goal FROM sessions WHERE id = ?")
        .bind(session_id)
        .fetch_optional(&state.pool)
        .await
        .map_err(|error| error.to_string())?
        .unwrap_or_default();
    let mut files: Vec<String> = state.hub.get_all_files(session_id).await.into_keys().collect();
    files.sort();
    let more_files = files.len() > PLANNER_MAX_FILES;
    files.truncate(PLANNER_MAX_FILES);
    let blueprint: String = state
        .hub
        .get_blueprint(session_id)
        .await
        .chars()
        .take(PLANNER_MAX_BLUEPRINT_CHARS)
        .collect();
    let previous = state.hub.get_tasks(session_id).await;
    let previous_tasks = previous
        .tasks
        .iter()
        .map(|task| format!("- [{}] {} ({})", task.status, task.title, task.assignee.as_deref().unwrap_or("unassigned")))
        .collect::<Vec<_>>()
        .join("\n");

    let context = format!(
        "Team (assign only to these user_ids):\n{team}\n\nGoal: {goal}\n\nDiscussion summary:\n{digest}\n\n\
         Most recent messages:\n{recent}\n\nFiles in the session:\n{files}{more}\n\nReference doc:\n{blueprint}\n\n\
         Current tasks (keep finished work finished; replace the rest):\n{previous_tasks}",
        goal = if goal.is_empty() { "(not set)" } else { goal.as_str() },
        digest = if digest.is_empty() { "(none yet)" } else { digest.as_str() },
        recent = format_transcript(&recent, &names, DIGEST_MAX_MESSAGE_CHARS),
        files = if files.is_empty() { "(none shared yet)".to_string() } else { files.join("\n") },
        more = if more_files { "\n… (more)" } else { "" },
        blueprint = if blueprint.is_empty() { "(none)" } else { blueprint.as_str() },
        previous_tasks = if previous_tasks.is_empty() { "(none)".to_string() } else { previous_tasks },
    );
    let model = std::env::var("AGENT_PLANNER_MODEL").unwrap_or_else(|_| agent::model_or_fallback());
    let mut conversation = vec![json!({"role": "user", "content": context})];
    let mut attempt = 0;
    let board = loop {
        attempt += 1;
        let (reply, cost) =
            agent::call_model(&state.http_client, &model, PLANNER_SYSTEM_PROMPT, conversation.clone()).await?;
        state.hub.record_llm_call(session_id, cost).await;
        match parse_plan(&reply, &names, &previous, requested_by) {
            Ok(board) => break board,
            // Models occasionally slip on JSON; telling them exactly what
            // was wrong almost always fixes it on the next try.
            Err(error) if attempt < PLANNER_ATTEMPTS => {
                conversation.push(json!({"role": "assistant", "content": reply}));
                conversation.push(json!({"role": "user", "content": format!(
                    "That reply couldn't be used: {error}. Reply again with only the JSON object, in the exact shape asked for."
                )}));
            }
            Err(error) => return Err(error),
        }
    };
    persist_board(state, session_id, &board).await?;
    state.hub.set_tasks(session_id, board.clone()).await;
    state
        .hub
        .broadcast(session_id, &json!({"type": "task_board", "board": board}))
        .await;
    agent::broadcast_agent_reply(state, session_id, board_announcement(&board, &names)).await;
    Ok(())
}

/// The board as prompt text for agents that didn't build it.
pub fn format_board(board: &TaskBoard) -> String {
    if board.tasks.is_empty() {
        return "(no tasks yet)".to_string();
    }
    board
        .tasks
        .iter()
        .map(|task| {
            format!(
                "- [{}] {} — {} — files: {}",
                task.status,
                task.title,
                task.assignee.as_deref().unwrap_or("unassigned"),
                if task.files.is_empty() { "-".to_string() } else { task.files.join(", ") }
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

async fn persist_board(state: &AppState, session_id: &str, board: &TaskBoard) -> Result<(), String> {
    let encoded = serde_json::to_string(board).map_err(|error| error.to_string())?;
    sqlx::query(
        "INSERT INTO session_tasks (session_id, board) VALUES (?, ?) \
         ON CONFLICT(session_id) DO UPDATE SET board = excluded.board, \
         updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')",
    )
    .bind(session_id)
    .bind(encoded)
    .execute(&state.pool)
    .await
    .map_err(|error| error.to_string())?;
    Ok(())
}

fn board_announcement(board: &TaskBoard, names: &HashMap<String, String>) -> String {
    let mut lines = vec![format!("**Task plan ready** — {} tasks. {}", board.tasks.len(), board.summary)];
    for task in &board.tasks {
        let owner = task
            .assignee
            .as_ref()
            .map(|user_id| names.get(user_id).cloned().unwrap_or_else(|| user_id.clone()))
            .unwrap_or_else(|| "unassigned".to_string());
        lines.push(format!("- **{owner}**: {}", task.title));
    }
    lines.join("\n")
}

/// Pulls the JSON object out of a model reply (models often wrap it in a
/// code fence or a sentence) and turns it into a board, resolving
/// assignees given by name as well as by id.
fn parse_plan(
    reply: &str,
    names: &HashMap<String, String>,
    previous: &TaskBoard,
    requested_by: &str,
) -> Result<TaskBoard, String> {
    let start = reply.find('{').ok_or("the plan had no JSON in it")?;
    let end = reply.rfind('}').ok_or("the plan had no JSON in it")?;
    if end < start {
        return Err("the plan had no JSON in it".to_string());
    }
    let raw: RawPlan = serde_json::from_str(&reply[start..=end])
        .map_err(|error| format!("the plan wasn't valid JSON: {error}"))?;
    let raw_tasks: Vec<RawTask> = raw
        .tasks
        .into_iter()
        .filter(|task| !task.title.trim().is_empty())
        .collect();
    if raw_tasks.is_empty() {
        return Err("the plan had no tasks".to_string());
    }

    let version = previous.version + 1;
    let ids: Vec<String> = (1..=raw_tasks.len()).map(|n| format!("t{version}-{n}")).collect();
    let resolve_assignee = |value: &str| -> Option<String> {
        let value = value.trim().trim_start_matches('@');
        if names.contains_key(value) {
            return Some(value.to_string());
        }
        names
            .iter()
            .find(|(_, name)| name.eq_ignore_ascii_case(value))
            .map(|(user_id, _)| user_id.clone())
    };
    let tasks = raw_tasks
        .iter()
        .enumerate()
        .map(|(index, raw_task)| {
            let title = raw_task.title.trim().to_string();
            let depends_on = raw_task
                .depends_on
                .iter()
                .filter_map(|dependency| match dependency {
                    serde_json::Value::Number(number) => {
                        let position = number.as_u64()? as usize;
                        ids.get(position.checked_sub(1)?).cloned()
                    }
                    serde_json::Value::String(text) => raw_tasks
                        .iter()
                        .position(|other| other.title.trim().eq_ignore_ascii_case(text.trim()))
                        .and_then(|position| ids.get(position).cloned()),
                    _ => None,
                })
                .filter(|dependency| dependency != &ids[index])
                .collect();
            // Work already underway or finished keeps its status across a
            // re-plan, as long as the planner kept the task.
            let status = previous
                .tasks
                .iter()
                .find(|old| old.title.eq_ignore_ascii_case(&title))
                .map(|old| old.status.clone())
                .unwrap_or_else(|| "todo".to_string());
            Task {
                id: ids[index].clone(),
                title,
                detail: raw_task.detail.trim().to_string(),
                assignee: raw_task.assignee.as_deref().and_then(resolve_assignee),
                files: raw_task
                    .files
                    .iter()
                    .map(|path| path.trim().trim_start_matches("./").to_string())
                    .filter(|path| !path.is_empty())
                    .collect(),
                depends_on,
                status,
                updated_by: None,
            }
        })
        .collect();
    Ok(TaskBoard {
        summary: raw.summary.trim().to_string(),
        tasks,
        version,
        generated_by: Some(requested_by.to_string()),
    })
}

/// The task's owner can move their own task along; whoever leads the
/// session (everyone, without a leader) can move any task.
pub async fn update_task_status(state: &AppState, session_id: &str, user_id: &str, task_id: &str, status: &str) {
    if !TASK_STATUSES.contains(&status) {
        return;
    }
    let leads = may_lead(state, session_id, user_id).await;
    let updated = state
        .hub
        .update_tasks(session_id, |board| {
            let task = board.tasks.iter_mut().find(|task| task.id == task_id)?;
            if !leads && task.assignee.as_deref() != Some(user_id) {
                return None;
            }
            task.status = status.to_string();
            task.updated_by = Some(user_id.to_string());
            Some(())
        })
        .await;
    let Some((Some(()), board)) = updated else {
        return;
    };
    if let Err(error) = persist_board(state, session_id, &board).await {
        eprintln!("team: storing task update in {session_id} failed: {error}");
    }
    state
        .hub
        .broadcast(session_id, &json!({"type": "task_board", "board": board}))
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names() -> HashMap<String, String> {
        HashMap::from([
            ("u-alice".to_string(), "alice".to_string()),
            ("u-bob".to_string(), "bob".to_string()),
        ])
    }

    #[test]
    fn parses_a_fenced_plan_and_resolves_names_and_dependencies() {
        let reply = "Here you go:\n```json\n{\"summary\": \"Split by layer\", \"tasks\": [\
            {\"title\": \"API\", \"detail\": \"Build it\", \"assignee\": \"u-bob\", \"files\": [\"./src/api.rs\"]},\
            {\"title\": \"UI\", \"description\": \"Screens\", \"assignee\": \"Alice\", \"depends_on\": [1, 2]}]}\n```";
        let board = parse_plan(reply, &names(), &TaskBoard::default(), "u-alice").unwrap();
        assert_eq!(board.version, 1);
        assert_eq!(board.summary, "Split by layer");
        assert_eq!(board.tasks[0].id, "t1-1");
        assert_eq!(board.tasks[0].assignee.as_deref(), Some("u-bob"));
        assert_eq!(board.tasks[0].files, vec!["src/api.rs".to_string()]);
        assert_eq!(board.tasks[1].assignee.as_deref(), Some("u-alice"));
        assert_eq!(board.tasks[1].detail, "Screens");
        // A task can't depend on itself.
        assert_eq!(board.tasks[1].depends_on, vec!["t1-1".to_string()]);
        assert!(board.tasks.iter().all(|task| task.status == "todo"));
    }

    #[test]
    fn unknown_assignees_are_left_unassigned() {
        let reply = r#"{"tasks": [{"title": "Docs", "assignee": "carol"}]}"#;
        let board = parse_plan(reply, &names(), &TaskBoard::default(), "u-alice").unwrap();
        assert_eq!(board.tasks[0].assignee, None);
    }

    #[test]
    fn replanning_keeps_progress_on_kept_tasks() {
        let previous = TaskBoard {
            version: 3,
            tasks: vec![Task {
                id: "t3-1".to_string(),
                title: "API".to_string(),
                detail: String::new(),
                assignee: Some("u-bob".to_string()),
                files: Vec::new(),
                depends_on: Vec::new(),
                status: "done".to_string(),
                updated_by: None,
            }],
            ..TaskBoard::default()
        };
        let reply = r#"{"tasks": [{"title": "api", "assignee": "bob"}, {"title": "Tests"}]}"#;
        let board = parse_plan(reply, &names(), &previous, "u-alice").unwrap();
        assert_eq!(board.version, 4);
        assert_eq!(board.tasks[0].status, "done");
        assert_eq!(board.tasks[1].status, "todo");
    }

    #[test]
    fn rejects_replies_without_tasks() {
        assert!(parse_plan("no json here", &names(), &TaskBoard::default(), "u").is_err());
        assert!(parse_plan(r#"{"tasks": []}"#, &names(), &TaskBoard::default(), "u").is_err());
    }

    #[test]
    fn file_owners_skip_finished_and_unassigned_tasks() {
        let task = |title: &str, assignee: Option<&str>, status: &str, file: &str| Task {
            id: title.to_string(),
            title: title.to_string(),
            detail: String::new(),
            assignee: assignee.map(str::to_string),
            files: vec![file.to_string()],
            depends_on: Vec::new(),
            status: status.to_string(),
            updated_by: None,
        };
        let board = TaskBoard {
            tasks: vec![
                task("a", Some("u-bob"), "in_progress", "a.rs"),
                task("b", Some("u-alice"), "done", "b.rs"),
                task("c", None, "todo", "c.rs"),
            ],
            ..TaskBoard::default()
        };
        let owners = board.file_owners();
        assert_eq!(owners.len(), 1);
        assert_eq!(owners.get("a.rs").map(String::as_str), Some("u-bob"));
    }

    #[test]
    fn transcript_names_speakers_and_truncates() {
        let messages = vec![
            ChatMessage { id: 1, sender: "u-alice".to_string(), text: "hello".to_string(), author: None },
            ChatMessage { id: 2, sender: "AI (Agent)".to_string(), text: "x".repeat(20), author: Some("u-bob".to_string()) },
        ];
        let transcript = format_transcript(&messages, &names(), 5);
        assert_eq!(transcript, "[alice] hello\n[AI (Agent) for bob] xxxxx…");
    }
}
