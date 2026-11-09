// Thin typed client for vibecut-web's REST + WS API. Every export
// here is a 1:1 mirror of a route in
// src/vibecut-web/src/routes/mod.rs — no logic lives here beyond
// request shaping and error unwrapping.

import type { FeedbackAction } from './types/generated/FeedbackAction';
import type { ChangeNote } from './types/generated/ChangeNote';
import type { Config } from './types/generated/Config';
import type { OutputTimeline } from './types/generated/OutputTimeline';
import type { Segment } from './types/generated/Segment';

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

async function getJson<T>(path: string, signal?: AbortSignal): Promise<T> {
	const res = await fetch(`${BASE}${path}`, { signal });
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

// ---------- health ----------

/** `ai`: whether the server can answer free-text briefs. */
export function health(): Promise<{ ok: boolean; service: string; ai?: boolean }> {
	return getJson('/health');
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

export interface FootageFolder {
	/** Relative to /footage — what discover/propose take. */
	folder: string;
	name: string;
	n_videos: number;
	total_bytes: number;
	newest: string | null;
	sample_video: string | null;
}

export function listFootage(): Promise<{ folders: FootageFolder[] }> {
	return getJson('/footage');
}

/** `signal` drops a scan the UI no longer wants (the server may still
 *  finish it). */
export function discoverSources(folder: string, signal?: AbortSignal): Promise<DiscoverResult> {
	return getJson(`/sources?folder=${encodeURIComponent(folder)}`, signal);
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
	/** Song file; omit to let the server pick one for the vibe, 'none' for no music. */
	soundtrack?: string;
}

export interface ProposeResult {
	vibe: string;
	vibe_name: string;
	rationale: string;
	config_path: string;
	config_toml: string;
	output_path: string;
	/** The song the proposed config uses (absent on proposals saved by older builds). */
	soundtrack?: ProposedSoundtrack | null;
	/** The server's pick for this vibe, length and seed: what "Let Vibecut pick" means. */
	auto_pick?: MusicPick | null;
	/** Why there's no music, when there isn't. */
	music_note?: string | null;
}

export interface ProposedSoundtrack {
	path: string;
	title: string;
	picked_by: 'vibecut' | 'you';
	reason: string | null;
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
	beat_sync?: {
		tempo_bpm: number | null;
		n_cuts: number | null;
		within_tolerance: number | null;
		mean_abs_error_ms: number | null;
	} | null;
}

export interface RenderResult {
	ok: boolean;
	config_path: string;
	error?: string;
	output_path?: string;
	output_exists?: boolean;
	manifest_path?: string;
	manifest_summary?: ManifestSummary;
	/** This render's own copy (async renders): later renders overwrite
	 *  `output_path`, never these. */
	version_path?: string;
	version_manifest_path?: string;
	/** `draft`: a fast 540p preview (`<stem>.draft.mp4`) of the same edit;
	 *  `final`: the full render. Absent from servers before drafts. */
	quality?: RenderQuality;
}

/** How finished a render is (see `[project].quality` in CONFIG.md). */
export type RenderQuality = 'draft' | 'final';

export function renderSync(configPath: string, quality?: RenderQuality): Promise<RenderResult> {
	return sendJson('/render', { config_path: configPath, ...(quality ? { quality } : {}) });
}

export function renderAsync(
	configPath: string,
	quality?: RenderQuality
): Promise<{ job_id: string; state: string; quality?: RenderQuality | null }> {
	return sendJson('/render/async', { config_path: configPath, ...(quality ? { quality } : {}) });
}

// ---------- plan (the edit before the encode) ----------

/** One frame to show for a timeline clip: grab `source_path` at `t_secs`. */
export interface PlanThumbnail {
	clip_index: number;
	segment_index: number;
	source_path: string;
	t_secs: number;
	output_secs: number;
}

/** `POST /api/plan`: the edit a render of the config makes, before any of
 *  it is encoded. `segments` and `timeline` have the manifest's shape. */
export interface EditPlan {
	config_path: string;
	/** `render`: the plan of the render of this config that is running. */
	planned_by: 'render' | 'plan';
	elapsed_ms: number;
	project_name: string;
	fps: number;
	seed: number | null;
	target_duration_secs: number;
	duration_secs: number;
	song?: { path: string; tempo_bpm: number; beat_interval_secs: number; duration_secs: number } | null;
	/** Beats as they will play in the video (seconds). */
	beats: number[];
	segments: Segment[];
	timeline: OutputTimeline;
	thumbnails: PlanThumbnail[];
}

export function planEdit(configPath: string): Promise<EditPlan> {
	return sendJson('/plan', { config_path: configPath });
}

/** Latest pipeline stage report for a running render (see
 *  `vibecut::progress`). `overall` never goes backwards. */
export interface RenderProgress {
	stage:
		| 'probing'
		| 'music'
		| 'analysing'
		| 'picking'
		| 'clips'
		| 'joining'
		| 'mixing'
		| 'encoding'
		| 'checking';
	label: string;
	stage_fraction: number;
	overall: number;
	detail: string | null;
}

export type JobStatus =
	| { job_id: string; state: 'queued' }
	| { job_id: string; state: 'running'; started_at: string; progress: RenderProgress | null }
	| ({ job_id: string; state: 'done'; started_at: string; finished_at: string; elapsed_ms: number } & RenderResult)
	| { job_id: string; state: 'failed'; started_at: string; finished_at: string; elapsed_ms: number; error: string }
	/** Stopped on request; `started_at` is null when it never left the queue. */
	| { job_id: string; state: 'cancelled'; started_at: string | null; finished_at: string; elapsed_ms: number };

export function renderStatus(jobId: string): Promise<JobStatus> {
	return getJson(`/render/${encodeURIComponent(jobId)}`);
}

// ---------- jobs ----------

/** What a job does: a full render, or a platform version of one. */
export type JobKind = 'render' | 'reformat';

interface JobListCommon {
	/** The project config a render renders; empty for reformats. */
	config_path: string;
	/** A finished render's quality (`draft` / `final`). */
	quality?: RenderQuality;
	submitted_at: string;
	/** A Stop is on its way (a running job turns `cancelled` within about a second). */
	cancel_requested: boolean;
}

export type RenderJobEntry = JobStatus & JobListCommon & { kind: 'render' };

/** A reformat job: the render it works from, its preset and fit, and the
 *  version file it writes (known before it is done). */
export type ReformatJobEntry = ReformatStatus &
	JobListCommon & {
		kind: 'reformat';
		source_path: string;
		preset: PlatformPreset;
		fit: 'blur' | 'crop' | 'pad' | null;
		output_path: string;
	};

/** One row of the jobs list (`GET /api/jobs`). */
export type JobListEntry = RenderJobEntry | ReformatJobEntry;

/** Recent jobs (renders and reformats), newest first. */
export function listJobs(): Promise<{ jobs: JobListEntry[] }> {
	return getJson('/jobs');
}

/** Stop a render. The previous output file is left untouched. */
export function cancelJob(jobId: string): Promise<JobListEntry> {
	return sendJson(`/jobs/${encodeURIComponent(jobId)}/cancel`, {});
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

/** Frames from any server-side video at given times — used with a
 *  source chapter path for footage thumbnails. */
export function inspectAt(videoPath: string, times: number[]): Promise<FrameSummary> {
	const t = times.map((x) => x.toFixed(2)).join(',');
	return getJson(`/frames/at?output_path=${encodeURIComponent(videoPath)}&t=${t}`);
}

// ---------- reformat (platform versions) ----------

/** Platform presets `POST /reformat` (and `[[outputs]].preset`) accept. */
export type PlatformPreset = 'youtube' | 'reels' | 'tiktok' | 'shorts' | 'square' | 'whatsapp' | 'master';

export interface ReformatResult {
	ok: boolean;
	source_path: string;
	/** The version, `<stem>.<preset>.mp4` next to the source. */
	output_path: string;
	preset: PlatformPreset;
	width: number;
	height: number;
	/** `null` when the source already has the preset's aspect. */
	fit: 'blur' | 'crop' | 'pad' | null;
	/** Stream-copied: the source already was this shape. */
	copied: boolean;
	/** An up-to-date version already existed. */
	cached: boolean;
	encoder: string | null;
	duration_secs: number;
	source_duration_secs: number;
	size_bytes: number;
}

/** Start a platform version of a finished render; poll `reformatStatus`.
 *  `reused`: a job was already making this version, and this is that job. */
export function reformat(
	outputPath: string,
	preset: PlatformPreset,
	fit?: 'blur' | 'crop' | 'pad'
): Promise<{
	job_id: string;
	kind: 'reformat';
	state: string;
	reused: boolean;
	preset: PlatformPreset;
	output_path: string;
}> {
	return sendJson('/reformat', { output_path: outputPath, preset, ...(fit ? { fit } : {}) });
}

export type ReformatStatus =
	| { job_id: string; state: 'queued' }
	| { job_id: string; state: 'running'; started_at: string; progress: RenderProgress | null }
	| ({ job_id: string; state: 'done'; started_at: string; finished_at: string; elapsed_ms: number } & ReformatResult)
	| { job_id: string; state: 'failed'; started_at: string; finished_at: string; elapsed_ms: number; error: string }
	/** Stopped (the Renders list's Stop); the last good version is untouched. */
	| { job_id: string; state: 'cancelled'; started_at: string | null; finished_at: string; elapsed_ms: number };

/** Reformats run as jobs in the same registry as renders. */
export function reformatStatus(jobId: string): Promise<ReformatStatus> {
	return getJson(`/render/${encodeURIComponent(jobId)}`);
}

// ---------- music library ----------

export interface Track {
	path: string;
	title: string;
	collection: 'yours' | 'library' | 'starter';
	artist: string | null;
	licence: TrackLicence;
	/** A public post must credit the track (CC BY and friends). */
	attribution_required: boolean;
	/** Ready-made credit line for captions. */
	credit: string | null;
	source_url: string | null;
	declared_bpm: number | null;
	/** Where the licence info came from: a `<track>.json` sidecar or the folder's library.json. */
	metadata: 'sidecar' | 'library' | null;
	/** Steady beat and a long loud section: repeats without an audible seam. */
	loops_cleanly: boolean;
	duration_secs: number;
	bpm: number;
	beat_interval_secs: number;
	first_beat_secs: number | null;
	grid_tight: boolean | null;
	pulse_strength: number;
	/** 48-bin loudness sketch, 0..1. */
	energy: number[];
	energy_mean: number;
	mood: 'chill' | 'warm' | 'upbeat' | 'driving';
	loud_region: [number, number];
}

export interface TrackLicence {
	/** Normalised id: 'CC0-1.0', 'CC-BY-4.0', 'Pixabay', 'YouTube-Audio-Library', 'unknown', … */
	id: string;
	label: string;
	kind:
		| 'unknown'
		| 'cc0'
		| 'cc_by'
		| 'cc_by_sa'
		| 'cc_by_nc'
		| 'cc_by_nd'
		| 'pixabay'
		| 'youtube_audio_library'
		| 'custom';
	/** Plain-language advice for posting a video with it. */
	note: string;
}

export interface MusicLibrary {
	tracks: Track[];
	/** Unreadable or invalid metadata files, unvetted starter-pack tracks. */
	warnings: string[];
	/** Where to put downloaded songs, as the server sees it. */
	import_dir: string;
	dirs: { path: string; collection: string; exists: boolean }[];
}

export function listMusic(): Promise<MusicLibrary> {
	return getJson('/music');
}

/** The server's song choice for a vibe, with the reason. */
export interface MusicPick {
	path: string;
	title: string;
	reason: string;
	score: number;
	close_matches: number;
	covers_length: boolean;
	loops_cleanly: boolean;
}

/** The song a proposal with these settings would auto-pick (null: empty library). */
export function pickMusic(args: { vibe: string; target_duration?: string; seed?: number }): Promise<{ pick: MusicPick | null }> {
	const q = new URLSearchParams({ vibe: args.vibe, seed: String(args.seed ?? 42) });
	if (args.target_duration) q.set('target_duration', args.target_duration);
	return getJson(`/music/pick?${q}`);
}

/** Stream URL for previewing a library track (supports seeking). */
export function musicFileUrl(path: string): string {
	return `${BASE}/music/file?path=${encodeURIComponent(path)}`;
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

// ---------- style codes (recipes) ----------
//
// A style code ("VC1-…") carries a video's creative choices — vibe,
// length, pacing, cuts, cards, song, formats — never its footage.

/** One thing a style sets, in words ("Length" · "30 s"). */
export interface StyleChip {
	label: string;
	value: string;
}

export interface StyleSong {
	/** Track id (12 hex digits): the same file matches on any machine. */
	id: string | null;
	title: string;
	artist: string | null;
	bpm: number | null;
}

export type StyleMusic = { kind: 'original' } | { kind: 'auto' } | { kind: 'songs'; songs: StyleSong[] };

/** A setting that differs from the vibe's own (`transition.duration` …). */
export interface StyleSetting {
	path: string;
	value?: unknown;
	remove?: boolean;
	count?: number;
}

export interface StyleSummary {
	vibe: string;
	vibe_name: string;
	/** As a config writes it ("1m 30s"). */
	target_duration: string;
	target_secs: number;
	seed: number;
	music: StyleMusic;
	chips: StyleChip[];
	settings: StyleSetting[];
}

/** Which song a style gets from this library, and why. */
export interface StyleMusicMatch {
	how: 'same_song' | 'same_title' | 'similar_tempo' | 'vibecut_pick' | 'original_sound' | 'no_music';
	/** The files the new config uses; empty for no music. */
	files: string[];
	title: string | null;
	note: string;
}

export interface StyleShare {
	code: string;
	chars: number;
	style: StyleSummary;
}

export interface StylePreview {
	code: string;
	style: StyleSummary;
	music_match: StyleMusicMatch;
}

/** A proposal made from a code: propose's result plus the style. */
export interface StyleProposal extends ProposeResult {
	code: string;
	style: StyleSummary;
	music_match: StyleMusicMatch;
}

export interface SavedStyle {
	name: string;
	code: string;
	file: string;
	created_at: string | null;
	style: StyleSummary;
}

/** The style of a project's config as a code. `manifest_path`: the shown
 *  version's, whose song credit and tempo fill in a song the library lacks. */
export function shareStyle(args: {
	config_path: string;
	manifest_path?: string;
	vibe?: string;
	let_vibecut_pick?: boolean;
}): Promise<StyleShare> {
	return sendJson('/recipes/code', args);
}

/** What a code (or a saved style's name) would make, and its song here. */
export function previewStyle(code: string): Promise<StylePreview> {
	return getJson(`/recipes/preview?code=${encodeURIComponent(code)}`);
}

/** A proposal for `folder` in that style — then the normal render. */
export function applyStyle(args: { code: string; folder: string; title?: string }): Promise<StyleProposal> {
	return sendJson('/recipes/apply', args);
}

export function listStyles(): Promise<{ styles: SavedStyle[]; warnings: string[]; dir: string }> {
	return getJson('/recipes');
}

/** Keep a code under a name (`/projects/recipes`); the same name replaces it. */
export function saveStyle(name: string, code: string): Promise<SavedStyle> {
	return sendJson('/recipes', { name, code });
}
