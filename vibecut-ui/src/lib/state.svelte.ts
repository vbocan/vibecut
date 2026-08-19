// Page-level reactive state for the Sources → Vibe → Config → Render
// → Preview flow. A singleton `.svelte.ts` module rather than
// prop-drilling — this is wizard-shaped, single-flow state, not
// something multiple independent page trees need isolated copies of.

import * as api from './api';
import type { FeedbackAction } from './api';
import type { Manifest } from './types/generated/Manifest';
import type { Segment } from './types/generated/Segment';

type RenderMode = 'idle' | 'queued' | 'running' | 'done' | 'failed';

/** One segment plus its approximate position on the OUTPUT timeline. */
export interface SegmentLayout {
	index: number;
	segment: Segment;
	startSecs: number;
	endSecs: number;
}

/** One completed render, tracked client-side for the A/B compare
 *  panel — see `AppState.renderHistory`'s doc comment for why this
 *  isn't derived from the JSONL session log instead. */
export interface RenderHistoryEntry {
	label: string;
	output_path: string;
	ts: number;
}

class AppState {
	// ---------- Sources ----------
	folder = $state('');
	discovering = $state(false);
	discoverResult = $state<api.DiscoverResult | null>(null);
	discoverError = $state<string | null>(null);

	async discover() {
		this.discovering = true;
		this.discoverError = null;
		try {
			this.discoverResult = await api.discoverSources(this.folder);
		} catch (e) {
			this.discoverError = e instanceof api.ApiError ? e.describe() : String(e);
		} finally {
			this.discovering = false;
		}
	}

	// ---------- Vibes ----------
	vibes = $state<api.VibeEntry[]>([]);
	vibesError = $state<string | null>(null);
	selectedVibe = $state<string | null>(null);
	title = $state('');
	targetDuration = $state('');
	seed = $state(42);
	proposing = $state(false);
	proposeResult = $state<api.ProposeResult | null>(null);
	proposeError = $state<string | null>(null);

	async loadVibes() {
		try {
			const r = await api.listVibes();
			this.vibes = r.vibes;
		} catch (e) {
			this.vibesError = e instanceof api.ApiError ? e.describe() : String(e);
		}
	}

	async propose() {
		if (!this.selectedVibe) return;
		this.proposing = true;
		this.proposeError = null;
		try {
			const r = await api.proposeVibe({
				folder: this.folder,
				vibe: this.selectedVibe,
				title: this.title || undefined,
				target_duration: this.targetDuration || undefined,
				seed: this.seed
			});
			this.proposeResult = r;
			this.configToml = r.config_toml;
			this.configDirty = false;
			// A fresh proposal invalidates whatever the previous one rendered.
			this.resetRender();
		} catch (e) {
			this.proposeError = e instanceof api.ApiError ? e.describe() : String(e);
		} finally {
			this.proposing = false;
		}
	}

	// ---------- Config editor ----------
	configToml = $state('');
	configDirty = $state(false);
	savingConfig = $state(false);
	configError = $state<string | null>(null);
	lastChanges = $state<api.ChangeNote[]>([]);
	feedbackBusy = $state(false);

	setConfigToml(text: string) {
		this.configToml = text;
		this.configDirty = this.proposeResult !== null && text !== this.proposeResult.config_toml;
	}

	/** True once the config has changed since the render currently
	 *  shown in Preview was produced — the fastest way to tell the
	 *  user "what you're looking at doesn't reflect your last edit
	 *  yet" without re-diffing the TOML on every keystroke. */
	configChangedSinceRender = $state(false);

	async saveConfig() {
		if (!this.proposeResult) return;
		this.savingConfig = true;
		this.configError = null;
		try {
			const r = await api.writeConfig(this.proposeResult.config_path, this.configToml);
			this.configToml = r.config_toml;
			this.configDirty = false;
			this.configChangedSinceRender = true;
		} catch (e) {
			this.configError = e instanceof api.ApiError ? e.describe() : String(e);
		} finally {
			this.savingConfig = false;
		}
	}

	async quickFeedback(action: FeedbackAction) {
		if (!this.proposeResult) return;
		this.feedbackBusy = true;
		this.configError = null;
		try {
			const r = await api.applyFeedback(this.proposeResult.config_path, action);
			this.lastChanges = r.changes;
			// apply_feedback returns a diff, not the new file — re-read so
			// the editor reflects what's actually on disk now.
			const refreshed = await api.readConfig(this.proposeResult.config_path);
			this.configToml = refreshed.config_toml;
			this.configDirty = false;
			this.configChangedSinceRender = true;
		} catch (e) {
			this.configError = e instanceof api.ApiError ? e.describe() : String(e);
		} finally {
			this.feedbackBusy = false;
		}
	}

	// ---------- Render ----------
	renderMode = $state<RenderMode>('idle');
	jobId = $state<string | null>(null);
	renderResult = $state<api.RenderResult | null>(null);
	renderError = $state<string | null>(null);
	private ws: WebSocket | null = null;

	private resetRender() {
		this.ws?.close();
		this.ws = null;
		this.renderMode = 'idle';
		this.jobId = null;
		this.renderResult = null;
		this.renderError = null;
		this.frames = [];
	}

	startRenderAsync() {
		if (!this.proposeResult) return;
		this.ws?.close();
		this.renderMode = 'queued';
		this.renderError = null;
		this.renderResult = null;
		this.frames = [];
		this.manifest = null;
		this.manifestError = null;
		this.selectedSegmentIndex = null;
		this.configChangedSinceRender = false;
		api
			.renderAsync(this.proposeResult.config_path)
			.then((r) => {
				this.jobId = r.job_id;
				this.openProgressSocket(r.job_id);
			})
			.catch((e) => {
				this.renderMode = 'failed';
				this.renderError = e instanceof api.ApiError ? e.describe() : String(e);
			});
	}

	private openProgressSocket(jobId: string) {
		const ws = api.renderProgressSocket(jobId);
		this.ws = ws;
		ws.onmessage = (ev) => {
			const data = JSON.parse(ev.data) as api.JobStatus | { error: string };
			// The unknown-job-id sentinel is the only shape here with no
			// `state` field — narrow on that rather than on `'error' in
			// data`, which TS can't disambiguate cleanly since the
			// `done` variant also carries an *optional* `error` from
			// `RenderResult`.
			if (!('state' in data)) {
				this.renderMode = 'failed';
				this.renderError = data.error;
				return;
			}
			this.renderMode = data.state;
			if (data.state === 'done') {
				this.renderResult = data;
				this.recordRenderHistory(data.output_path);
				void this.loadFrames();
				void this.loadManifest();
			} else if (data.state === 'failed') {
				this.renderError = data.error;
			}
		};
		// If the socket itself errors (proxy down, etc.) the job may
		// still be running server-side — a manual refresh via
		// render_status stays available; this is a scaffold, not a
		// reconnect-with-backoff implementation.
	}

	// ---------- Preview ----------
	frames = $state<api.FrameInline[]>([]);
	loadingFrames = $state(false);
	frameError = $state<string | null>(null);

	async loadFrames() {
		const path = this.renderResult?.output_path;
		if (!path) return;
		this.loadingFrames = true;
		this.frameError = null;
		try {
			const r = await api.inspectOutput(path, 12);
			this.frames = r.frames_inline;
		} catch (e) {
			this.frameError = e instanceof api.ApiError ? e.describe() : String(e);
		} finally {
			this.loadingFrames = false;
		}
	}

	// ---------- Timeline ----------
	manifest = $state<Manifest | null>(null);
	manifestError = $state<string | null>(null);
	loadingManifest = $state(false);
	selectedSegmentIndex = $state<number | null>(null);

	async loadManifest() {
		const path = this.renderResult?.manifest_path;
		if (!path) return;
		this.loadingManifest = true;
		this.manifestError = null;
		try {
			// The manifest is just another file under /projects — reuse
			// the same file-serving endpoint the video player uses
			// rather than adding a dedicated "get manifest" route.
			const res = await fetch(api.outputFileUrl(path));
			if (!res.ok) throw new Error(`manifest fetch failed: HTTP ${res.status}`);
			this.manifest = (await res.json()) as Manifest;
		} catch (e) {
			this.manifestError = e instanceof Error ? e.message : String(e);
		} finally {
			this.loadingManifest = false;
		}
	}

	/**
	 * Segments laid out along the OUTPUT timeline by cumulative
	 * `duration_secs` — deliberately the same approximation
	 * `apply_drop_segment` already uses server-side (intro cards and
	 * per-seam transition/speed-ramp overlap aren't in the manifest at
	 * all). It won't always land on the exact rendered frame, but it
	 * identifies the same segment the server would for the same
	 * `at_seconds`, which is the property that actually matters for
	 * the feedback actions below.
	 */
	segmentLayout = $derived.by((): SegmentLayout[] => {
		if (!this.manifest) return [];
		let cum = 0;
		return this.manifest.segments.map((segment, index) => {
			const startSecs = cum;
			cum += segment.duration_secs;
			return { index, segment, startSecs, endSecs: cum };
		});
	});

	segmentAt(timeSecs: number): SegmentLayout | null {
		return (
			this.segmentLayout.find((s) => timeSecs >= s.startSecs && timeSecs < s.endSecs) ?? null
		);
	}

	async dropSegment(layout: SegmentLayout) {
		const midpoint = (layout.startSecs + layout.endSecs) / 2;
		await this.quickFeedback({
			kind: 'drop_segment',
			at_seconds: midpoint,
			manifest_path: this.renderResult?.manifest_path ?? null
		});
		this.selectedSegmentIndex = null;
	}

	async moreOfSource(sourceId: string) {
		await this.quickFeedback({ kind: 'more_of', source_ids: [sourceId] });
	}

	async lessOfSource(sourceId: string) {
		await this.quickFeedback({ kind: 'less_of', source_ids: [sourceId] });
	}

	// ---------- Free-text brief ----------
	briefText = $state('');
	briefBusy = $state(false);
	briefError = $state<string | null>(null);
	briefRationale = $state<string | null>(null);

	/**
	 * One text box, two modes, chosen by whether a proposal exists yet
	 * — mirrors the MCP conversation shape ("propose a vibe, then
	 * react in plain language") without needing an LLM client in the
	 * loop: this calls the Anthropic API directly from
	 * `vibecut-web`, then routes the structured result through the
	 * exact same `propose()`/`quickFeedback()` calls the manual
	 * UI controls use, so there is exactly one code path that ever
	 * writes a config either way.
	 */
	async submitBrief() {
		const text = this.briefText.trim();
		if (!text || this.briefBusy) return;
		this.briefBusy = true;
		this.briefError = null;
		this.briefRationale = null;
		try {
			if (!this.proposeResult) {
				const r = await api.submitBriefPropose(text);
				if (r.mode !== 'propose') throw new Error('unexpected brief response for propose mode');
				this.briefRationale = r.rationale;
				this.selectedVibe = r.vibe;
				if (r.title) this.title = r.title;
				if (r.target_duration) this.targetDuration = r.target_duration;
				await this.propose();
			} else {
				const r = await api.submitBriefFeedback(text, this.configToml);
				if (r.mode !== 'feedback') throw new Error('unexpected brief response for feedback mode');
				this.briefRationale = r.rationale;
				await this.quickFeedback(r.action);
			}
			this.briefText = '';
		} catch (e) {
			this.briefError =
				e instanceof api.ApiError ? e.describe() : e instanceof Error ? e.message : String(e);
		} finally {
			this.briefBusy = false;
		}
	}

	// ---------- History / A-B compare ----------
	sessions = $state<api.SessionFileInfo[]>([]);
	sessionsError = $state<string | null>(null);
	loadingSessions = $state(false);

	async loadSessions() {
		this.loadingSessions = true;
		this.sessionsError = null;
		try {
			const r = await api.listSessions();
			this.sessions = r.sessions;
		} catch (e) {
			this.sessionsError = e instanceof api.ApiError ? e.describe() : String(e);
		} finally {
			this.loadingSessions = false;
		}
	}

	/**
	 * Renders produced in THIS browser session — the compare panel's
	 * source of truth. Deliberately not derived from the JSONL session
	 * log: reconstructing "which renders exist and what are their
	 * output paths" from arbitrary MCP-or-web call history is real
	 * parsing work for uncertain payoff, when this session's own
	 * renders already are what "did my last tweak actually change
	 * anything?" needs to compare against.
	 */
	renderHistory = $state<RenderHistoryEntry[]>([]);
	compareA = $state<number | null>(null);
	compareB = $state<number | null>(null);

	private recordRenderHistory(outputPath: string | undefined) {
		if (!outputPath) return;
		const entry: RenderHistoryEntry = {
			label: `render ${this.renderHistory.length + 1}`,
			output_path: outputPath,
			ts: Date.now()
		};
		this.renderHistory = [...this.renderHistory, entry];
		this.compareB = this.renderHistory.length - 1;
		this.compareA = this.renderHistory.length >= 2 ? this.renderHistory.length - 2 : null;
	}
}

export const appState = new AppState();
