// Thin typed client for vibecut-web's REST + WS API. Every export
// here is a 1:1 mirror of a route in
// src/vibecut-web/src/routes/mod.rs — no logic lives here beyond
// request shaping and error unwrapping.

import type { FeedbackAction } from './types/generated/FeedbackAction';
import type { ChangeNote } from './types/generated/ChangeNote';
import type { Config } from './types/generated/Config';

const BASE = '/api';

export class ApiError extends Error {
	status: number;
	hint?: string;
	constructor(status: number, message: string, hint?: string) {
		super(message);
		this.status = status;
		this.hint = hint;
	}

	/** Message + hint on one line, ready to show the user. */
	describe(): string {
		return this.hint ? `${this.message} — ${this.hint}` : this.message;
	}
}

async function toApiError(res: Response): Promise<ApiError> {
	try {
		const body = await res.json();
		return new ApiError(res.status, body.error ?? res.statusText, body.hint);
	} catch {
		return new ApiError(res.status, res.statusText);
	}
}

async function getJson<T>(path: string): Promise<T> {
	const res = await fetch(`${BASE}${path}`);
	if (!res.ok) throw await toApiError(res);
	return res.json() as Promise<T>;
}

async function sendJson<T>(
	path: string,
	body: unknown,
	method: 'POST' | 'PUT' = 'POST'
): Promise<T> {
	const res = await fetch(`${BASE}${path}`, {
		method,
		headers: { 'content-type': 'application/json' },
		body: JSON.stringify(body)
	});
	if (!res.ok) throw await toApiError(res);
	return res.json() as Promise<T>;
}

// ---------- discover ----------

export interface ChapterSummary {
	path: string;
	chapter_index: number;
	duration_secs: number;
}

export interface SourceSummary {
	source_id: string;
	chapters: ChapterSummary[];
	n_chapters: number;
	total_duration_secs: number;
	has_audio: boolean;
	fps: number[];
	codecs: string[];
}

export interface DiscoverResult {
	folder: string;
	sources: SourceSummary[];
	n_sources: number;
	total_secs: number;
	summary: string;
	hint?: string;
}

export function discoverSources(folder: string): Promise<DiscoverResult> {
	return getJson(`/sources?folder=${encodeURIComponent(folder)}`);
}

// ---------- vibes ----------

export interface VibeEntry {
	key: string;
	name: string;
	rationale: string;
	default_target_duration: string;
}

export function listVibes(): Promise<{ vibes: VibeEntry[] }> {
	return getJson('/vibes');
}

// ---------- propose ----------

export interface ProposeArgs {
	folder: string;
	vibe: string;
	title?: string;
	target_duration?: string;
	seed?: number;
}

export interface ProposeResult {
	vibe: string;
	vibe_name: string;
	rationale: string;
	config_path: string;
	config_toml: string;
	output_path: string;
}

export function proposeVibe(args: ProposeArgs): Promise<ProposeResult> {
	return sendJson('/propose', args);
}

// ---------- config ----------

export interface ConfigText {
	config_path: string;
	config_toml: string;
}

export function readConfig(configPath: string): Promise<ConfigText> {
	return getJson(`/config?config_path=${encodeURIComponent(configPath)}`);
}

export function writeConfig(configPath: string, configToml: string): Promise<ConfigText> {
	return sendJson('/config', { config_path: configPath, config_toml: configToml }, 'PUT');
}

// ---------- config (structured) ----------
//
// Same file as above, typed instead of text — what the guided
// (wizard) editor reads and writes. No TOML crosses this boundary in
// either direction; `Config` is generated from the exact Rust struct
// `vibecut::project_config::Config` deserializes, via ts-rs.

export type { Config };

export function readConfigStructured(configPath: string): Promise<Config> {
	return getJson(`/config/structured?config_path=${encodeURIComponent(configPath)}`);
}

export function writeConfigStructured(configPath: string, config: Config): Promise<Config> {
	return sendJson('/config/structured', { config_path: configPath, config }, 'PUT');
}

// ---------- feedback ----------

export interface ApplyFeedbackResult {
	ok: boolean;
	config_path: string;
	changes: ChangeNote[];
	n_changes: number;
}

export function applyFeedback(
	configPath: string,
	action: FeedbackAction
): Promise<ApplyFeedbackResult> {
	return sendJson('/feedback', { config_path: configPath, action });
}

export type { FeedbackAction, ChangeNote };

// ---------- render ----------

export interface ManifestSummary {
	n_segments: number;
	actual_duration_secs: number | null;
	target_duration_secs: number | null;
	n_sources: number;
}

export interface RenderResult {
	ok: boolean;
	config_path: string;
	error?: string;
	output_path?: string;
	output_exists?: boolean;
	manifest_path?: string;
	manifest_summary?: ManifestSummary;
}

export function renderSync(configPath: string): Promise<RenderResult> {
	return sendJson('/render', { config_path: configPath });
}

export function renderAsync(configPath: string): Promise<{ job_id: string; state: string }> {
	return sendJson('/render/async', { config_path: configPath });
}

export type JobStatus =
	| { job_id: string; state: 'queued' }
	| { job_id: string; state: 'running'; started_at: string }
	| ({ job_id: string; state: 'done'; started_at: string; finished_at: string; elapsed_ms: number } & RenderResult)
	| { job_id: string; state: 'failed'; started_at: string; finished_at: string; elapsed_ms: number; error: string };

export function renderStatus(jobId: string): Promise<JobStatus> {
	return getJson(`/render/${encodeURIComponent(jobId)}`);
}

/** Opens the push-based progress feed. Caller owns the socket's lifecycle. */
export function renderProgressSocket(jobId: string): WebSocket {
	const proto = location.protocol === 'https:' ? 'wss:' : 'ws:';
	return new WebSocket(`${proto}//${location.host}${BASE}/ws/render/${encodeURIComponent(jobId)}`);
}

// ---------- frames ----------

export interface FrameInline {
	t_secs: number;
	data_uri: string;
}

export interface FrameError {
	t_secs: number;
	error: string;
}

export interface FrameSummary {
	output_path: string;
	duration_secs: number;
	frame_width_px: number;
	frames: { t_secs: number; size_bytes: number }[];
	errors: FrameError[];
	frames_inline: FrameInline[];
}

export function inspectOutput(outputPath: string, maxFrames = 12): Promise<FrameSummary> {
	return getJson(`/frames/output?output_path=${encodeURIComponent(outputPath)}&max_frames=${maxFrames}`);
}

// ---------- output file ----------

/** URL a `<video>`/`<a>` can point at directly to stream the file. */
export function outputFileUrl(path: string): string {
	return `${BASE}/output?path=${encodeURIComponent(path)}`;
}

// ---------- brief ----------

export interface BriefProposeResult {
	mode: 'propose';
	vibe: string;
	title: string | null;
	target_duration: string | null;
	rationale: string;
}

export interface BriefFeedbackResult {
	mode: 'feedback';
	action: FeedbackAction;
	rationale: string;
}

export type BriefResult = BriefProposeResult | BriefFeedbackResult;

export function submitBriefPropose(brief: string): Promise<BriefResult> {
	return sendJson('/brief', { mode: 'propose', brief });
}

export function submitBriefFeedback(brief: string, configToml: string): Promise<BriefResult> {
	return sendJson('/brief', { mode: 'feedback', brief, config_toml: configToml });
}

// ---------- sessions ----------

export interface SessionFileInfo {
	filename: string;
	path: string;
	size_bytes: number;
	modified: string | null;
}

export function listSessions(): Promise<{ sessions: SessionFileInfo[] }> {
	return getJson('/sessions');
}
