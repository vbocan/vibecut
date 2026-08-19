//! `POST /api/brief` — free-text brief → structured intent, via a
//! direct Anthropic API call.
//!
//! Two modes, chosen by the caller based on whether a proposal exists
//! yet: `propose` picks a vibe + starting params; `feedback` picks a
//! `FeedbackAction` against the current config. Either way this
//! endpoint only translates text to structure — the frontend still
//! calls the existing `/api/propose` / `/api/feedback` endpoints to
//! actually act on the result, so there's exactly one code path that
//! ever mutates a config file, same as the quick-feedback buttons.

use axum::Json;
use serde::{Deserialize, Serialize};
use vibecut_ops::feedback::{feedback_pick_json_schema, FeedbackAction, FeedbackPick};
use vibecut_ops::propose::list_vibes;

use crate::error::ApiError;
use crate::llm;

#[derive(Debug, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum BriefRequest {
    Propose { brief: String },
    Feedback { brief: String, config_toml: String },
}

#[derive(Debug, Serialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum BriefResponse {
    Propose {
        vibe: String,
        title: Option<String>,
        target_duration: Option<String>,
        rationale: String,
    },
    Feedback {
        action: FeedbackAction,
        rationale: String,
    },
}

pub async fn submit_brief(Json(body): Json<BriefRequest>) -> Result<Json<BriefResponse>, ApiError> {
    match body {
        BriefRequest::Propose { brief } => propose_mode(&brief).await,
        BriefRequest::Feedback { brief, config_toml } => feedback_mode(&brief, &config_toml).await,
    }
}

async fn propose_mode(brief: &str) -> Result<Json<BriefResponse>, ApiError> {
    let vibes = list_vibes();
    let keys: Vec<&str> = vibes
        .iter()
        .filter_map(|v| v.get("key").and_then(|k| k.as_str()))
        .collect();
    let catalog: String = vibes
        .iter()
        .map(|v| {
            format!(
                "- {}: {} (default duration {})",
                v.get("key").and_then(|k| k.as_str()).unwrap_or(""),
                v.get("rationale").and_then(|r| r.as_str()).unwrap_or(""),
                v.get("default_target_duration").and_then(|d| d.as_str()).unwrap_or(""),
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    let system = format!(
        "You are picking a starting \"vibe\" template for a Vibecut highlight-reel project, \
         based on the user's free-text brief. Available vibes:\n\n{catalog}\n\n\
         Pick the vibe whose rationale best matches the brief. Only set title or \
         target_duration if the brief actually implies them — leave them null otherwise \
         so the vibe's own sensible default applies."
    );
    // Plain string enum, no nested/referenced types — safe to
    // hand-roll directly, unlike FeedbackPick's schema (see its doc
    // comment for why that one has to come from a single schemars call).
    let schema = serde_json::json!({
        "type": "object",
        "properties": {
            "vibe": { "type": "string", "enum": keys },
            "title": { "type": ["string", "null"] },
            "target_duration": {
                "type": ["string", "null"],
                "description": "humantime string, e.g. \"3m\" or \"90s\""
            },
            "rationale": {
                "type": "string",
                "description": "one sentence explaining the pick, shown to the user"
            }
        },
        "required": ["vibe", "rationale"]
    });

    #[derive(Debug, Deserialize)]
    struct Pick {
        vibe: String,
        title: Option<String>,
        target_duration: Option<String>,
        rationale: String,
    }

    let pick: Pick = llm::call_tool(
        &system,
        brief,
        "pick_vibe",
        "Pick a vibe and starting parameters for this Vibecut project.",
        schema,
    )
    .await?;

    Ok(Json(BriefResponse::Propose {
        vibe: pick.vibe,
        title: pick.title,
        target_duration: pick.target_duration,
        rationale: pick.rationale,
    }))
}

async fn feedback_mode(brief: &str, config_toml: &str) -> Result<Json<BriefResponse>, ApiError> {
    let system = format!(
        "You are translating a user's plain-language reaction into ONE structured mutation \
         of their Vibecut project config. Pick the single feedback-action variant that \
         best matches what they said — each variant's description tells you which \
         natural-language reactions it covers. Current config:\n\n{config_toml}"
    );
    let schema = feedback_pick_json_schema();

    let pick: FeedbackPick = llm::call_tool(
        &system,
        brief,
        "pick_feedback_action",
        "Pick the feedback action that matches the user's reaction to their current cut.",
        schema,
    )
    .await?;

    Ok(Json(BriefResponse::Feedback {
        action: pick.action,
        rationale: pick.rationale,
    }))
}
