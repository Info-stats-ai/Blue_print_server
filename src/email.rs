//! Session invite emails, via Resend's HTTP API.
//!
//! Dormant until configured, same pattern as `oauth.rs`'s provider
//! handlers and `agent.rs`'s missing-API-key path: `send_invite` checks
//! `RESEND_API_KEY`/`INVITE_FROM_EMAIL` first and returns a clear error
//! instead of silently no-op'ing or building a malformed request. A
//! deployment that hasn't set these up yet just can't serve this one
//! endpoint; everything else (including sharing a session id by hand,
//! which this is a convenience wrapper around, not a replacement for)
//! keeps working regardless.
//!
//! There's no custom URL scheme (`i-ide://join-session/...`) registered
//! with the OS for this app, so the email can't be a single magic link
//! that opens the IDE straight into the session — that would need
//! platform-level URL-scheme registration in the packaged app bundle,
//! untested and out of scope here. Instead the email carries the same
//! session id a person would otherwise be copy-pasted, with plain
//! instructions for where to paste it.

use anyhow::Context as _;
use serde_json::json;

/// Sends `invitee_email` an invite to join `session_name` (id
/// `session_id`), from `inviter_email`, via Resend
/// (<https://resend.com/docs/api-reference/emails/send-email>). Returns an
/// error (never a malformed/half-sent request) if `RESEND_API_KEY` or
/// `INVITE_FROM_EMAIL` aren't set.
pub async fn send_invite(
    client: &reqwest::Client,
    invitee_email: &str,
    inviter_email: &str,
    session_name: &str,
    session_id: &str,
) -> anyhow::Result<()> {
    let api_key = std::env::var("RESEND_API_KEY").context("RESEND_API_KEY is not set")?;
    let from = std::env::var("INVITE_FROM_EMAIL").context("INVITE_FROM_EMAIL is not set")?;

    let subject = format!("{inviter_email} invited you to \u{201c}{session_name}\u{201d} on I");
    let text = format!(
        "{inviter_email} invited you to collaborate on \u{201c}{session_name}\u{201d} in the I IDE.\n\n\
         Open this link on a computer with the I IDE installed to join:\n\n\
         i-ide://i-mode/join/{session_id}\n\n\
         If the link doesn't open the app, choose \u{201c}Join session\u{2026}\u{201d} from the people menu \
         next to the I-mode switch and paste the link or this invite code:\n\n\
         {session_id}\n"
    );

    let response = client
        .post("https://api.resend.com/emails")
        .bearer_auth(api_key)
        .json(&json!({
            "from": from,
            "to": [invitee_email],
            "subject": subject,
            "text": text,
        }))
        .send()
        .await
        .context("failed to reach the email provider")?;

    if !response.status().is_success() {
        let body = response.text().await.unwrap_or_default();
        anyhow::bail!("email provider rejected the invite: {body}");
    }
    Ok(())
}

/// Tells the site's owner that `signup` joined the waitlist (`total` people
/// so far). Sent to `WAITLIST_NOTIFY_EMAIL`; skipped quietly when that isn't
/// set, and an error when it is but sending isn't set up.
pub async fn notify_waitlist_signup(client: &reqwest::Client, signup: &str, total: i64) -> anyhow::Result<()> {
    let Some(to) = std::env::var("WAITLIST_NOTIFY_EMAIL").ok().filter(|to| !to.trim().is_empty()) else {
        return Ok(());
    };
    let api_key = std::env::var("RESEND_API_KEY").context("RESEND_API_KEY is not set")?;
    let from = std::env::var("INVITE_FROM_EMAIL").context("INVITE_FROM_EMAIL is not set")?;
    let response = client
        .post("https://api.resend.com/emails")
        .bearer_auth(api_key)
        .json(&json!({
            "from": from,
            "to": [to.trim()],
            "reply_to": signup,
            "subject": format!("New waitlist signup: {signup}"),
            "text": format!("{signup} joined the I waitlist.\n\nThat's {total} {} so far. Reply to this email to write to them.\n", if total == 1 { "person" } else { "people" }),
        }))
        .send()
        .await
        .context("failed to reach the email provider")?;
    if !response.status().is_success() {
        let body = response.text().await.unwrap_or_default();
        anyhow::bail!("email provider rejected the waitlist notice: {body}");
    }
    Ok(())
}

/// Welcomes `signup` to the waitlist. Sent once per address (the signup
/// handler only calls it for a new one), from `INVITE_FROM_EMAIL`.
pub async fn send_waitlist_welcome(client: &reqwest::Client, signup: &str) -> anyhow::Result<()> {
    let api_key = std::env::var("RESEND_API_KEY").context("RESEND_API_KEY is not set")?;
    let from = std::env::var("INVITE_FROM_EMAIL").context("INVITE_FROM_EMAIL is not set")?;
    let site = std::env::var("PUBLIC_BASE_URL").unwrap_or_else(|_| "https://i-ide.dev".to_string());
    let mut message = json!({
        "from": from,
        "to": [signup],
        "subject": "You're on the I waitlist",
        "text": format!(
            "Thanks for joining the waitlist for I, the code editor where your whole team and every AI work in sync.\n\n\
             We'll email you as soon as your spot opens up. Until then, you can read how it works at {site}/docs.\n\n\
             If you didn't sign up, you can ignore this email; you won't hear from us again.\n"
        ),
    });
    if let Some(reply_to) = std::env::var("WAITLIST_NOTIFY_EMAIL").ok().filter(|to| !to.trim().is_empty()) {
        message["reply_to"] = json!(reply_to.trim());
    }
    let response = client
        .post("https://api.resend.com/emails")
        .bearer_auth(api_key)
        .json(&message)
        .send()
        .await
        .context("failed to reach the email provider")?;
    if !response.status().is_success() {
        let body = response.text().await.unwrap_or_default();
        anyhow::bail!("email provider rejected the welcome email: {body}");
    }
    Ok(())
}
