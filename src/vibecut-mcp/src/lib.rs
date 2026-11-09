//! MCP server library for Vibecut.
//!
//! Every operation this
//! server exposes — discover, list/propose vibes, apply feedback,
//! render (sync + async + status), inspect frames — lives in
//! `vibecut-ops` as plain transport-free async functions. This
//! crate is now a thin wrapper: each `#[tool]` deserialises MCP
//! arguments, calls straight into `vibecut_ops`, and packages the
//! result as MCP wants it (a JSON string for most tools, inline
//! `Content::image` for the two multimodal `inspect_*` tools). A
//! future `vibecut-web` crate calls the exact same
//! `vibecut_ops` functions and packages them as REST/WebSocket
//! responses instead — neither transport duplicates the pipeline
//! logic, both sit on the same substrate.

use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::*,
    schemars::JsonSchema,
    tool, tool_handler, tool_router, ServerHandler,
};
use serde::{Deserialize, Serialize};
use vibecut_ops::error::OpsError;
use vibecut_ops::{discover, feedback, frames, jobs, logger, propose, render};

#[derive(Clone)]
pub struct VibecutMcpServer {
    tool_router: ToolRouter<Self>,
    /// Session log. Each tool call appends one JSONL line to
    /// `/projects/sessions/<container_iso>.jsonl`. Best-effort —
    /// disabled silently if the path can't be created.
    logger: logger::SessionLogger,
    /// Fire-and-fetch job registry, now living in
    /// `vibecut-ops` so a future web UI can submit/poll renders
    /// through the same registry.
    jobs: jobs::JobRegistry,
}

impl Default for VibecutMcpServer {
    fn default() -> Self {
        Self::new()
    }
}

// ---------- tool argument structs (deserialised by rmcp) ----------

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct DiscoverArgs {
    /// Folder name relative to the container's `/footage` mount, or an
    /// absolute container path. Pass `""` (or omit) to scan the whole
    /// `/footage` mount.
    #[serde(default)]
    pub folder: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct ProposeArgs {
    /// Footage folder (same convention as `discover_sources`).
    #[serde(default)]
    pub folder: String,

    /// Which curated vibe to start from. Available: `kid_reel`,
    /// `family_chronicle`, `drone_cinematic`, `action_highlight`,
    /// `documentary`, `memory_keeper`. Use `list_vibes` to see
    /// rationales.
    #[serde(default = "default_vibe")]
    pub vibe: String,

    /// Optional intro-card title. Defaults to `"<folder> · YYYY-MM-DD"`
    /// when omitted.
    pub title: Option<String>,

    /// Target output duration as a humantime string: `"3m"`, `"90s"`,
    /// `"1m30s"`. When omitted, the vibe's own default is used (e.g.
    /// drone_cinematic defaults to "2m", memory_keeper to "8m").
    pub target_duration: Option<String>,

    /// Picker seed for reproducibility — same seed + same TOML always
    /// produces the same cut.
    #[serde(default = "default_seed")]
    pub seed: u64,

    /// Soundtrack file (container path, e.g. under `/projects/music`).
    /// Omit to let Vibecut pick a library song that suits the vibe and
    /// length (the result says which and why); `"none"` for no music.
    pub soundtrack: Option<String>,
}

fn default_vibe() -> String {
    "kid_reel".to_string()
}
fn default_seed() -> u64 {
    42
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct RenderArgs {
    /// Path to a TOML config file inside the container — typically the
    /// `config_path` returned by `propose_vibe`. Absolute or relative to
    /// `/projects`.
    pub config_path: String,
    /// `draft`: a fast 540p preview of the same edit, written beside the
    /// final as `<stem>.draft.mp4` (seconds rather than minutes). `final`
    /// (default) renders every output in full quality.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quality: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct PlanArgs {
    /// Path to a TOML config file inside the container (absolute, or
    /// relative to `/projects`).
    pub config_path: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct RenderStatusArgs {
    /// Job ID returned by `render_async`.
    pub job_id: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct InspectOutputArgs {
    /// Path to a rendered mp4 (typically the `output_path` from the
    /// most recent `render`).
    pub output_path: String,
    /// How many frames to extract. Default 12. Capped at 16 to keep
    /// the response compact.
    pub max_frames: Option<u32>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct InspectAtArgs {
    /// Path to a rendered mp4.
    pub output_path: String,
    /// Output-time positions (in seconds) to extract frames at.
    /// Negative or out-of-range values are silently dropped.
    pub timestamps: Vec<f32>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct ReformatArgs {
    /// Path to a rendered mp4 (the `output_path` of a `render`).
    pub output_path: String,
    /// `youtube`, `reels`, `tiktok`, `shorts`, `square` or `whatsapp`.
    pub preset: String,
    /// `blur` (default), `crop` or `pad`.
    #[serde(default)]
    pub fit: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct ApplyFeedbackArgs {
    /// Path to the TOML config to mutate (typically the
    /// `config_path` returned by the most recent `propose_vibe` or
    /// `apply_feedback`).
    pub config_path: String,
    /// What the user wants to change. Pick the variant that best maps
    /// to the user's reaction. See the variant descriptions in the
    /// schema for the natural-language patterns each one covers.
    pub action: feedback::FeedbackAction,
}

// ---------- the server ----------

#[tool_router]
impl VibecutMcpServer {
    pub fn new() -> Self {
        Self {
            tool_router: Self::tool_router(),
            logger: logger::SessionLogger::open("vibecut-mcp", env!("CARGO_PKG_VERSION")),
            jobs: jobs::JobRegistry::new(),
        }
    }

    /// Records the result of a string-returning tool
    /// (discover, list_vibes, propose, apply_feedback, render,
    /// render_async, render_status) with a curated excerpt extracted
    /// from the JSON body. Best-effort; never propagates errors.
    fn log_string_call(
        &self,
        tool: &str,
        args: serde_json::Value,
        result: &Result<String, ErrorData>,
        started: std::time::Instant,
    ) {
        let elapsed_ms = started.elapsed().as_millis() as u64;
        let (excerpt, ok) = match result {
            Ok(s) => (
                serde_json::from_str::<serde_json::Value>(s)
                    .map(|v| excerpt_for(tool, &v))
                    .unwrap_or_else(|_| {
                        serde_json::json!({
                            "raw_text_preview": s.chars().take(200).collect::<String>()
                        })
                    }),
                true,
            ),
            Err(e) => (
                serde_json::json!({
                    "error_code": format!("{:?}", e.code),
                    "error_message": format!("{}", e.message),
                }),
                false,
            ),
        };
        self.logger.record(tool, args, excerpt, elapsed_ms, ok);
    }

    /// Helper for the multimodal `inspect_*` tools that return
    /// `CallToolResult`. Logs the count of returned frames and the
    /// total inline byte size — never the JPEG bytes themselves.
    fn log_inspect_call(
        &self,
        tool: &str,
        args: serde_json::Value,
        result: &Result<rmcp::model::CallToolResult, ErrorData>,
        started: std::time::Instant,
    ) {
        let elapsed_ms = started.elapsed().as_millis() as u64;
        let (excerpt, ok) = match result {
            Ok(call_result) => {
                let mut n_images = 0usize;
                let mut total_b64_bytes: usize = 0;
                let mut summary_text: Option<serde_json::Value> = None;
                for c in &call_result.content {
                    if let Some(img) = c.as_image() {
                        n_images += 1;
                        total_b64_bytes += img.data.len();
                    } else if let Some(txt) = c.as_text() {
                        if summary_text.is_none() {
                            // The first text block is the JSON summary.
                            summary_text =
                                serde_json::from_str(&txt.text).ok();
                        }
                    }
                }
                (
                    serde_json::json!({
                        "n_frames": n_images,
                        "total_inline_bytes": total_b64_bytes,
                        "summary": summary_text,
                    }),
                    true,
                )
            }
            Err(e) => (
                serde_json::json!({
                    "error_code": format!("{:?}", e.code),
                    "error_message": format!("{}", e.message),
                }),
                false,
            ),
        };
        self.logger.record(tool, args, excerpt, elapsed_ms, ok);
    }

    /// Scan a folder of GoPro / DJI footage and summarise what's there.
    /// Use this as the first step of any conversation: it tells you how
    /// many sources are present, total duration, whether they have
    /// audio, and groups GoPro chapters (`GH010023.MP4`,
    /// `GH020023.MP4`, …) into one logical source.
    #[tool(description = "Scan a folder of GoPro / DJI footage; return per-source summary including chapter grouping, total duration, and audio availability.")]
    async fn discover_sources(
        &self,
        Parameters(args): Parameters<DiscoverArgs>,
    ) -> Result<String, ErrorData> {
        let started = std::time::Instant::now();
        let args_json = serde_json::to_value(&args).unwrap_or(serde_json::Value::Null);
        let result = self.discover_sources_impl(args).await;
        self.log_string_call("discover_sources", args_json, &result, started);
        result
    }

    async fn discover_sources_impl(&self, args: DiscoverArgs) -> Result<String, ErrorData> {
        match discover::discover_sources(&args.folder).await {
            Ok(r) => Ok(serde_json::to_string_pretty(&r).unwrap_or_else(|_| "{}".into())),
            Err(e) => ops_error_to_string_result(e),
        }
    }

    /// List the curated vibes the server can propose. Use this when the
    /// user hasn't picked a vibe yet — the rationale field of each
    /// entry is the cue you use to decide which two or three to surface
    /// to the user given what's in their footage.
    #[tool(description = "List the curated vibe library: key, name, rationale, default target duration. Use this to decide which vibes to suggest based on what discover_sources found.")]
    async fn list_vibes(&self) -> Result<String, ErrorData> {
        let started = std::time::Instant::now();
        let result = self.list_vibes_impl().await;
        self.log_string_call("list_vibes", serde_json::Value::Null, &result, started);
        result
    }

    async fn list_vibes_impl(&self) -> Result<String, ErrorData> {
        Ok(serde_json::to_string_pretty(&serde_json::json!({
            "vibes": propose::list_vibes(),
        }))
        .unwrap_or_else(|_| "{}".into()))
    }

    /// Generate a starting TOML config for the named vibe and footage
    /// folder. The TOML is written to `/projects` so the user can edit
    /// it; pass the returned `config_path` to `render` to produce the
    /// edit.
    #[tool(description = "Generate a starting TOML config for a vibe + footage folder. Writes the TOML to /projects and returns its path, contents, and rationale. Music is on by default: without `soundtrack` it picks a library song that suits the vibe and says why (`soundtrack.reason`).")]
    async fn propose_vibe(
        &self,
        Parameters(args): Parameters<ProposeArgs>,
    ) -> Result<String, ErrorData> {
        let started = std::time::Instant::now();
        let args_json = serde_json::to_value(&args).unwrap_or(serde_json::Value::Null);
        let result = self.propose_vibe_impl(args).await;
        self.log_string_call("propose_vibe", args_json, &result, started);
        result
    }

    async fn propose_vibe_impl(&self, args: ProposeArgs) -> Result<String, ErrorData> {
        let result = propose::propose_vibe(propose::ProposeArgs {
            folder: &args.folder,
            vibe: &args.vibe,
            title: args.title.as_deref(),
            target_duration: args.target_duration.as_deref(),
            seed: args.seed,
            soundtrack: args.soundtrack.as_deref(),
        })
        .await;
        match result {
            Ok(r) => {
                let body = serde_json::json!({
                    "vibe": r.vibe,
                    "vibe_name": r.vibe_name,
                    "rationale": r.rationale,
                    "config_path": r.config_path,
                    "config_toml": r.config_toml,
                    "output_path": r.output_path,
                    "soundtrack": r.soundtrack,
                    "auto_pick": r.auto_pick,
                    "music_note": r.music_note,
                    "next_step": "Call `render` with this config_path to produce the edit. To tweak before rendering, modify the TOML and save it back to the same path."
                });
                Ok(serde_json::to_string_pretty(&body).unwrap_or_else(|_| "{}".into()))
            }
            Err(e) => ops_error_to_string_result(e),
        }
    }

    /// Extract a curated set of overview frames from a rendered mp4
    /// and return them inline as MCP image content. Use this right
    /// after `render` so you can SEE what the cut looks like before
    /// reacting. Frames are evenly distributed across the timeline,
    /// JPEG-encoded at ~480 px wide for a small response.
    ///
    /// For a specific moment ("the seam at 0:42"), use `inspect_at`
    /// with explicit timestamps instead.
    #[tool(description = "Extract a curated overview of frames from a rendered mp4. Returns ~12 evenly-spaced frames as inline JPEG content for multimodal reasoning.")]
    async fn inspect_output(
        &self,
        Parameters(args): Parameters<InspectOutputArgs>,
    ) -> Result<rmcp::model::CallToolResult, ErrorData> {
        let started = std::time::Instant::now();
        let args_json = serde_json::to_value(&args).unwrap_or(serde_json::Value::Null);
        let result = self.inspect_output_impl(args).await;
        self.log_inspect_call("inspect_output", args_json, &result, started);
        result
    }

    async fn inspect_output_impl(
        &self,
        args: InspectOutputArgs,
    ) -> Result<rmcp::model::CallToolResult, ErrorData> {
        let result = frames::inspect_output(&args.output_path, args.max_frames)
            .await
            .map_err(ops_error_to_error_data)?;
        Ok(rmcp::model::CallToolResult::success(build_frame_content(result)))
    }

    /// Extract frames at user-specified timestamps. Use when the user
    /// references a specific moment ("the seam at 0:42", "what's at the
    /// end?") so the LLM can see and reason about that exact frame.
    /// Up to 16 timestamps per call.
    #[tool(description = "Extract frames at specific output-time timestamps. Returns each requested frame inline as JPEG. Use when the user references a specific moment.")]
    async fn inspect_at(
        &self,
        Parameters(args): Parameters<InspectAtArgs>,
    ) -> Result<rmcp::model::CallToolResult, ErrorData> {
        let started = std::time::Instant::now();
        let args_json = serde_json::to_value(&args).unwrap_or(serde_json::Value::Null);
        let result = self.inspect_at_impl(args).await;
        self.log_inspect_call("inspect_at", args_json, &result, started);
        result
    }

    async fn inspect_at_impl(
        &self,
        args: InspectAtArgs,
    ) -> Result<rmcp::model::CallToolResult, ErrorData> {
        let result = frames::inspect_at(&args.output_path, &args.timestamps)
            .await
            .map_err(ops_error_to_error_data)?;
        Ok(rmcp::model::CallToolResult::success(build_frame_content(result)))
    }

    /// A platform version of a finished render (vertical Reel, square,
    /// WhatsApp) in one fast ffmpeg pass — no re-render.
    #[tool(description = "Make a platform version of a finished render in one fast pass (seconds, no re-render): preset youtube (16:9 1920x1080), reels / tiktok / shorts (9:16 1080x1920), square (1080x1080) or whatsapp (9:16 720x1280, size-capped). A different aspect gets a blurred fill of the same frame behind it (fit: blur, default), a centre crop (crop) or black bars (pad). Writes <stem>.<preset>.mp4 next to the render and returns its path.")]
    async fn reformat(&self, Parameters(args): Parameters<ReformatArgs>) -> Result<String, ErrorData> {
        let started = std::time::Instant::now();
        let args_json = serde_json::to_value(&args).unwrap_or(serde_json::Value::Null);
        let result =
            match vibecut_ops::reformat::reformat_sync(&args.output_path, &args.preset, args.fit.as_deref()).await {
                Ok(body) => Ok(serde_json::to_string_pretty(&body).unwrap_or_else(|_| "{}".into())),
                Err(e) => ops_error_to_string_result(e),
            };
        self.log_string_call("reformat", args_json, &result, started);
        result
    }

    /// Mutate a TOML config in response to user feedback, then save it
    /// back to the same path. Does NOT re-render — the caller chains
    /// this with a follow-up `render` call when the user is ready.
    ///
    /// Pick the variant
    /// that best matches what the user said; the server applies a
    /// deterministic, type-safe mutation against the existing `Config`
    /// and tells you what fields changed.
    #[tool(description = "Apply user feedback to a TOML config and save it. Returns the field-level diff. Does NOT re-render — chain a `render` call when the user is ready.")]
    async fn apply_feedback(
        &self,
        Parameters(args): Parameters<ApplyFeedbackArgs>,
    ) -> Result<String, ErrorData> {
        let started = std::time::Instant::now();
        let args_json = serde_json::to_value(&args).unwrap_or(serde_json::Value::Null);
        let result = self.apply_feedback_impl(args).await;
        self.log_string_call("apply_feedback", args_json, &result, started);
        result
    }

    async fn apply_feedback_impl(&self, args: ApplyFeedbackArgs) -> Result<String, ErrorData> {
        match feedback::apply_feedback_to_file(&args.config_path, &args.action).await {
            Ok(r) => {
                let body = serde_json::json!({
                    "ok": true,
                    "config_path": r.config_path,
                    "action": &args.action,
                    "changes": r.changes,
                    "n_changes": r.changes.len(),
                    "next_step": if r.changes.is_empty() {
                        "Action was a no-op for this config (e.g. Calmer on a config that already had speed-ramps disabled). Pick a different action or render as-is."
                    } else {
                        "Call `render` with this config_path to produce the updated edit."
                    },
                });
                Ok(serde_json::to_string_pretty(&body).unwrap_or_else(|_| "{}".into()))
            }
            Err(e) => ops_error_to_string_result(e),
        }
    }

    /// Plan a config without rendering it: the edit the render will make,
    /// in seconds — every clip's source window and place in the output,
    /// the beats, the length. Lets the user see (and change) the cut before
    /// waiting for an encode.
    #[tool(description = "Plan the edit of a TOML config without rendering it (seconds on footage analysed before): the exact output timeline (each clip's source file, source in/out, output start/end, crossfades, speed ramps), the song's beats as they will play, the total duration, and a source time per clip for `inspect_at` thumbnails. While a render of the same config runs, returns that render's own plan.")]
    async fn plan_edit(&self, Parameters(args): Parameters<PlanArgs>) -> Result<String, ErrorData> {
        let started = std::time::Instant::now();
        let args_json = serde_json::to_value(&args).unwrap_or(serde_json::Value::Null);
        let result = match vibecut_ops::plan::plan(&args.config_path).await {
            Ok(body) => Ok(serde_json::to_string_pretty(&body).unwrap_or_else(|_| "{}".into())),
            Err(e) => ops_error_to_string_result(e),
        };
        self.log_string_call("plan_edit", args_json, &result, started);
        result
    }

    /// Submit a render job and return its `job_id` immediately.
    /// The render runs in the background; poll progress with
    /// `render_status(job_id)`.
    #[tool(description = "Submit a render in the background. Returns a job_id immediately. Poll `render_status(job_id)` until state is `done` or `failed`.")]
    async fn render_async(
        &self,
        Parameters(args): Parameters<RenderArgs>,
    ) -> Result<String, ErrorData> {
        let started = std::time::Instant::now();
        let args_json = serde_json::to_value(&args).unwrap_or(serde_json::Value::Null);
        let result = self.render_async_impl(args).await;
        self.log_string_call("render_async", args_json, &result, started);
        result
    }

    async fn render_async_impl(&self, args: RenderArgs) -> Result<String, ErrorData> {
        let req = match render::RenderRequest::from_quality_field(args.quality.as_deref()) {
            Ok(r) => r,
            Err(e) => return ops_error_to_string_result(e),
        };
        match render::submit_async_with(&self.jobs, &args.config_path, req).await {
            Ok(job_id) => Ok(serde_json::to_string_pretty(&serde_json::json!({
                "job_id": job_id,
                "state": "queued",
                "config_path": args.config_path,
                "next_step": "Poll `render_status` with this job_id until state is `done` or `failed`. Renders typically take 1-10 minutes; checking once a minute is plenty.",
            }))
            .unwrap_or_default()),
            Err(e) => ops_error_to_string_result(e),
        }
    }

    /// Poll the state of a render job submitted via
    /// `render_async`. States are `queued` / `running` / `done` /
    /// `failed` / `cancelled`; for `done` the response includes the same manifest
    /// summary the synchronous `render` tool returns.
    #[tool(description = "Poll the state of a job submitted via render_async. Returns queued/running/done/failed/cancelled plus the manifest summary when done.")]
    async fn render_status(
        &self,
        Parameters(args): Parameters<RenderStatusArgs>,
    ) -> Result<String, ErrorData> {
        let started = std::time::Instant::now();
        let args_json = serde_json::to_value(&args).unwrap_or(serde_json::Value::Null);
        let result = self.render_status_impl(args).await;
        self.log_string_call("render_status", args_json, &result, started);
        result
    }

    async fn render_status_impl(&self, args: RenderStatusArgs) -> Result<String, ErrorData> {
        let st = match render::render_status(&self.jobs, &args.job_id).await {
            Some(s) => s,
            None => {
                return Ok(json_error(
                    &format!("unknown job_id: {}", args.job_id),
                    Some("If you just submitted, give it ~50 ms; otherwise it never existed or was evicted."),
                ))
            }
        };
        // Shared with the REST `/api/render/{job_id}` endpoint and the
        // WebSocket progress feed in vibecut-web — one JSON shape
        // per job state regardless of transport.
        let body = render::job_state_json(&args.job_id, st);
        Ok(serde_json::to_string_pretty(&body).unwrap_or_default())
    }

    /// Stop a job submitted via `render_async`: a queued one never
    /// starts, a running one has its ffmpeg processes killed and turns
    /// `cancelled` within about a second. The last good output stays.
    #[tool(description = "Stop a job submitted via render_async. A queued job never starts; a running one is killed and reports `cancelled` within about a second. The previous output file is left untouched.")]
    async fn cancel_render(
        &self,
        Parameters(args): Parameters<RenderStatusArgs>,
    ) -> Result<String, ErrorData> {
        let started = std::time::Instant::now();
        let args_json = serde_json::to_value(&args).unwrap_or(serde_json::Value::Null);
        let result = match render::cancel(&self.jobs, &args.job_id).await {
            Some(body) => Ok(serde_json::to_string_pretty(&body).unwrap_or_default()),
            None => Ok(json_error(&format!("unknown job_id: {}", args.job_id), None)),
        };
        self.log_string_call("cancel_render", args_json, &result, started);
        result
    }

    /// Render a TOML config to an mp4. Blocking; takes 1–10 minutes.
    /// Returns the output path, manifest summary (segment count, actual
    /// vs target duration), and a status. Runs the pipeline in-process
    /// through `vibecut::run_pipeline::run_cmd` via `vibecut-ops`
    /// — no subprocess, no stdout-parse fragility.
    #[tool(description = "Run the vibecut pipeline on a TOML config. Blocking; takes 1-10 minutes. Returns output path, manifest summary, and exit status. Use `render_async` if you'd rather not wait.")]
    async fn render(
        &self,
        Parameters(args): Parameters<RenderArgs>,
    ) -> Result<String, ErrorData> {
        let started = std::time::Instant::now();
        let args_json = serde_json::to_value(&args).unwrap_or(serde_json::Value::Null);
        let result = self.render_impl(args).await;
        self.log_string_call("render", args_json, &result, started);
        result
    }

    async fn render_impl(&self, args: RenderArgs) -> Result<String, ErrorData> {
        let req = match render::RenderRequest::from_quality_field(args.quality.as_deref()) {
            Ok(r) => r,
            Err(e) => return ops_error_to_string_result(e),
        };
        match render::render_sync_with(&args.config_path, req).await {
            Ok(body) => Ok(serde_json::to_string_pretty(&body).unwrap_or_else(|_| "{}".into())),
            Err(e) => ops_error_to_string_result(e),
        }
    }
}

#[tool_handler]
impl ServerHandler for VibecutMcpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            protocol_version: ProtocolVersion::V_2025_03_26,
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            server_info: Implementation::from_build_env(),
            ..Default::default()
        }
    }
}

// ---------- OpsError → MCP translation ----------
//
// vibecut-ops has no opinion on transport; these two functions are
// the MCP-specific half of that boundary. `NotFound` / `InvalidInput`
// are caller-facing conditions the LLM can react to in conversation
// (missing folder, unknown vibe, bad path) — for the string-returning
// tools those surface as a soft `Ok(json_error(...))` body rather than
// a protocol-level error, exactly as they did before the extraction
// into `vibecut-ops`. The two multimodal `inspect_*` tools return
// `CallToolResult` instead of a JSON string, so there's no "soft body"
// to return the error inside — those map to `INVALID_PARAMS` directly.
// `Internal` always means "the tool call itself failed," so it always
// becomes an `ErrorData` in both mappings.

fn ops_error_to_string_result(e: OpsError) -> Result<String, ErrorData> {
    match e {
        OpsError::NotFound { what, detail } => {
            Ok(json_error(&format!("{what} not found"), detail.as_deref()))
        }
        OpsError::InvalidInput { message, hint } => Ok(json_error(&message, hint.as_deref())),
        OpsError::Internal(msg) => Err(ErrorData::new(ErrorCode::INTERNAL_ERROR, msg, None)),
    }
}

fn ops_error_to_error_data(e: OpsError) -> ErrorData {
    match e {
        OpsError::NotFound { what, detail } => ErrorData::new(
            ErrorCode::INVALID_PARAMS,
            match detail {
                Some(d) => format!("{what} not found: {d}"),
                None => format!("{what} not found"),
            },
            None,
        ),
        OpsError::InvalidInput { message, hint } => ErrorData::new(
            ErrorCode::INVALID_PARAMS,
            match hint {
                Some(h) => format!("{message} — {h}"),
                None => message,
            },
            None,
        ),
        OpsError::Internal(msg) => ErrorData::new(ErrorCode::INTERNAL_ERROR, msg, None),
    }
}

/// Package a `vibecut_ops::frames::FrameExtractionResult` as MCP
/// content: a JSON-text summary first, then one `Content::image` per
/// successfully extracted frame — the transport-specific half of frame
/// packaging that `vibecut-ops` deliberately stays agnostic to.
fn build_frame_content(result: frames::FrameExtractionResult) -> Vec<Content> {
    let summary_text =
        serde_json::to_string_pretty(&result.summary_json()).unwrap_or_else(|_| "{}".into());
    let mut out = Vec::with_capacity(result.frames.len() + 1);
    out.push(Content::text(summary_text));
    for f in &result.frames {
        let b64 = B64.encode(&f.jpeg_bytes);
        out.push(Content::image(b64, "image/jpeg"));
    }
    out
}

// ---------- session-log excerpt extraction ----------
//
// Each tool produces a JSON body; the logger records a curated subset
// keyed by tool name. The subset is what the replay harness compares
// to detect determinism drift, so it should NOT include things like
// timestamps or random IDs that change every run.

fn excerpt_for(tool: &str, body: &serde_json::Value) -> serde_json::Value {
    use serde_json::json;
    match tool {
        "discover_sources" => {
            let n = body.get("n_sources").cloned().unwrap_or(json!(null));
            let secs = body.get("total_secs").cloned().unwrap_or(json!(null));
            let summary = body.get("summary").cloned().unwrap_or(json!(null));
            let n_with_audio = body
                .get("sources")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter(|s| {
                            s.get("has_audio").and_then(|h| h.as_bool()) == Some(true)
                        })
                        .count()
                })
                .unwrap_or(0);
            json!({
                "n_sources": n,
                "total_secs": secs,
                "n_with_audio": n_with_audio,
                "summary": summary,
            })
        }
        "list_vibes" => {
            let count = body
                .get("vibes")
                .and_then(|v| v.as_array())
                .map(|a| a.len())
                .unwrap_or(0);
            json!({ "n_vibes": count })
        }
        "propose_vibe" => json!({
            "vibe": body.get("vibe").cloned().unwrap_or(json!(null)),
            "config_path": body.get("config_path").cloned().unwrap_or(json!(null)),
            "output_path": body.get("output_path").cloned().unwrap_or(json!(null)),
        }),
        "apply_feedback" => json!({
            "ok": body.get("ok").cloned().unwrap_or(json!(false)),
            "n_changes": body.get("n_changes").cloned().unwrap_or(json!(0)),
            // Keep the change list — small, replay-relevant.
            "changes": body.get("changes").cloned().unwrap_or(json!([])),
            "action": body.get("action").cloned().unwrap_or(json!(null)),
        }),
        "render" => json!({
            "ok": body.get("ok").cloned().unwrap_or(json!(false)),
            "output_path": body.get("output_path").cloned().unwrap_or(json!(null)),
            "manifest_summary": body
                .get("manifest_summary")
                .cloned()
                .unwrap_or(json!(null)),
        }),
        _ => {
            // Unknown tool: keep the whole body as long as it's small.
            // Truncate text fields > 500 chars to keep the log line lean.
            truncate_strings(body.clone(), 500)
        }
    }
}

fn truncate_strings(v: serde_json::Value, max_len: usize) -> serde_json::Value {
    match v {
        serde_json::Value::String(s) if s.len() > max_len => {
            serde_json::Value::String(format!("{}…", &s[..max_len]))
        }
        serde_json::Value::Array(arr) => serde_json::Value::Array(
            arr.into_iter().map(|x| truncate_strings(x, max_len)).collect(),
        ),
        serde_json::Value::Object(obj) => serde_json::Value::Object(
            obj.into_iter()
                .map(|(k, v)| (k, truncate_strings(v, max_len)))
                .collect(),
        ),
        other => other,
    }
}

// ---------- helpers ----------

fn json_error(message: &str, hint: Option<&str>) -> String {
    let mut v = serde_json::json!({ "error": message });
    if let Some(h) = hint {
        v["hint"] = serde_json::Value::String(h.to_string());
    }
    serde_json::to_string_pretty(&v).unwrap_or_else(|_| String::from(r#"{"error":"unknown"}"#))
}
