// Page-level reactive state for the Sources → Vibe → Config → Render
// → Preview flow. A singleton `.svelte.ts` module rather than
// prop-drilling — this is wizard-shaped, single-flow state, not
// something multiple independent page trees need isolated copies of.

import * as api from './api';
import type { FeedbackAction } from './api';
import type { Manifest } from './types/generated/Manifest';
import type { Segment } from './types/generated/Segment';
import type { Config } from './types/generated/Config';

type RenderMode = 'idle' | 'queued' | 'running' | 'done' | 'failed';

/** One segment plus its position on the OUTPUT timeline. */
export interface SegmentLayout {
	index: number;
	segment: Segment;
	/** Where the segment plays in the rendered video: exact (from the
	 *  manifest's timeline, or its beat-sync cut list) when available,
	 *  else approximate. */
	startSecs: number;
	endSecs: number;
	/** Cumulative-duration position — what the server's drop-segment
	 *  feedback uses to identify a segment in manifests without a
	 *  timeline, so feedback on those must use these. */
	approxStartSecs: number;
	approxEndSecs: number;
}

/** Each segment's visible span `[cut in, cut out)` from the manifest's
 *  exact timeline (a folded beat-burst has one entry per burst cut, all
 *  with its segment index), or null when the manifest has no timeline or
 *  it doesn't cover every segment. */
function timelineSpans(manifest: Manifest, n: number): [number, number][] | null {
	const clips = manifest.timeline?.clips;
	if (!clips?.length) return null;
	const spans: [number, number][] = Array.from({ length: n }, () => [Infinity, -Infinity]);
	for (const c of clips) {
		const s = spans[c.segment_index];
		if (!s) return null;
		s[0] = Math.min(s[0], c.cut_in_secs);
		s[1] = Math.max(s[1], c.cut_out_secs);
	}
	return spans.every(([a, b]) => b > a) ? spans : null;
}

/** One completed render, tracked client-side for the A/B compare
 *  panel — see `AppState.renderHistory`'s doc comment for why this
 *  isn't derived from the JSONL session log instead. */
export interface RenderHistoryEntry {
	label: string;
	output_path: string;
	manifest_path: string | null;
	ts: number;
	vibe: string | null;
	song: string | null;
	duration_secs: number | null;
	beats_locked: string | null;
}

/** What survives a page reload (localStorage). */
interface Persisted {
	v: 1;
	folder: string;
	selectedVibe: string | null;
	selectedTrackPath: string | null;
	proposeResult: api.ProposeResult | null;
	renderResult: api.RenderResult | null;
	renderHistory: RenderHistoryEntry[];
}

const STORAGE_KEY = 'vibecut.session.v1';

/** Vibe → the moods that suit it, best first (for auto-picking a song). */
const VIBE_MOODS: Record<string, api.Track['mood'][]> = {
	kid_reel: ['upbeat', 'driving', 'warm'],
	action_highlight: ['driving', 'upbeat'],
	drone_cinematic: ['chill', 'warm'],
	family_chronicle: ['warm', 'upbeat', 'chill'],
	documentary: ['chill', 'warm'],
	memory_keeper: ['warm', 'chill']
};

/** Where the one-tap flow is. */
export type MakeStep = 'idle' | 'scanning' | 'planning' | 'scoring' | 'rendering' | 'done' | 'failed';

class AppState {
	// ---------- Server ----------
	/** Server can answer free-text briefs (ANTHROPIC_API_KEY set). */
	aiAvailable = $state(false);

	async loadHealth() {
		try {
			this.aiAvailable = (await api.health()).ai === true;
		} catch {
			this.aiAvailable = false;
		}
	}

	// ---------- Footage folders ----------
	footage = $state<api.FootageFolder[]>([]);
	loadingFootage = $state(false);
	footageError = $state<string | null>(null);
	/** folder → poster frame (data URI). */
	folderPosters = $state<Record<string, string>>({});

	async loadFootage() {
		this.loadingFootage = true;
		this.footageError = null;
		try {
			this.footage = (await api.listFootage()).folders;
			// Zero decisions by default: start on the newest footage.
			if (!this.folder && this.footage.length > 0) {
				const newest = [...this.footage].sort((a, b) => (b.newest ?? '').localeCompare(a.newest ?? ''))[0];
				this.selectFolder(newest.folder);
			}
			void this.loadFolderPosters();
		} catch (e) {
			this.footageError = e instanceof api.ApiError ? e.describe() : String(e);
		} finally {
			this.loadingFootage = false;
		}
	}

	private async loadFolderPosters() {
		await runLimited(
			this.footage.filter((f) => f.sample_video && !this.folderPosters[f.folder]),
			3,
			async (f) => {
				try {
					const r = await api.inspectAt(f.sample_video!, [2]);
					const uri = r.frames_inline[0]?.data_uri;
					if (uri) this.folderPosters = { ...this.folderPosters, [f.folder]: uri };
				} catch {
					/* a missing poster is cosmetic */
				}
			}
		);
	}

	/** Folder (as passed to discover) that `discoverResult` describes. */
	discoveredFolder = $state<string | null>(null);

	selectFolder(folder: string) {
		if (this.discoveredFolder === folder && this.discoverResult) return;
		this.folder = folder;
		void this.discover();
	}

	// ---------- Source thumbnails ----------
	/** source_id → up to three frames across its first chapter. */
	sourceThumbs = $state<Record<string, string[]>>({});

	private async loadSourceThumbs() {
		const sources = this.discoverResult?.sources ?? [];
		await runLimited(sources, 3, async (src) => {
			const ch = src.chapters[0];
			if (!ch || this.sourceThumbs[src.source_id]) return;
			const d = ch.duration_secs;
			try {
				const r = await api.inspectAt(ch.path, [d * 0.15, d * 0.5, d * 0.85]);
				const frames = r.frames_inline.map((f) => f.data_uri);
				if (frames.length) this.sourceThumbs = { ...this.sourceThumbs, [src.source_id]: frames };
			} catch {
				/* cosmetic */
			}
		});
	}

	// ---------- Music library ----------
	tracks = $state<api.Track[]>([]);
	loadingMusic = $state(false);
	musicError = $state<string | null>(null);
	/** Chosen song; null = let Vibecut pick one that suits the vibe. */
	selectedTrackPath = $state<string | null>(null);

	async loadMusic() {
		this.loadingMusic = true;
		this.musicError = null;
		try {
			this.tracks = (await api.listMusic()).tracks;
		} catch (e) {
			this.musicError = e instanceof api.ApiError ? e.describe() : String(e);
		} finally {
			this.loadingMusic = false;
		}
	}

	/** Best library song for a vibe: matching mood first, then the
	 *  clearest beat (strongest pulse). Deterministic. */
	autoTrackFor(vibeKey: string | null): api.Track | null {
		if (this.tracks.length === 0) return null;
		const moods = (vibeKey && VIBE_MOODS[vibeKey]) || ['upbeat', 'warm', 'driving', 'chill'];
		const rank = (t: api.Track) => {
			const m = moods.indexOf(t.mood);
			return (m === -1 ? moods.length : m) * 100 - Math.min(t.pulse_strength, 50);
		};
		return [...this.tracks].sort((a, b) => rank(a) - rank(b) || a.path.localeCompare(b.path))[0];
	}

	/** The song a render will use: the chosen one, else the auto pick. */
	effectiveTrack = $derived.by((): api.Track | null => {
		if (this.selectedTrackPath) {
			return this.tracks.find((t) => t.path === this.selectedTrackPath) ?? null;
		}
		return this.autoTrackFor(this.effectiveVibe);
	});

	// ---------- One-tap "make my video" ----------
	makeStep = $state<MakeStep>('idle');
	makeError = $state<string | null>(null);
	renderProgress = $state<api.RenderProgress | null>(null);
	renderStartedAt = $state<number | null>(null);

	/**
	 * Scan (if needed) → propose the vibe → set the song → render.
	 * Every step goes through the same calls the advanced controls use,
	 * so the config it writes is an ordinary, editable proposal.
	 */
	async makeVideo(opts: { bed?: boolean } = {}) {
		if (!this.folder || !this.effectiveVibe) return;
		this.makeError = null;
		// Start the song now, inside the tap (autoplay rules), so the wait
		// has the music in it; the finished reel takes over when it's ready.
		if (opts.bed !== false) this.startBed(this.effectiveTrack);
		// A Stop before the render starts (or a newer run) bumps `makeRun`:
		// this run then quits at its next step instead of rendering.
		const run = ++this.makeRun;
		const live = () => run === this.makeRun;
		// A stopped run may still be waiting on the server: let its request
		// land first, so it can't overwrite this run's proposal.
		await this.makeInFlight;
		if (!live()) return;
		let landed!: () => void;
		this.makeInFlight = new Promise<void>((r) => (landed = () => r()));
		try {
			if (!this.discoverResult || this.discoveredFolder !== this.folder) {
				this.makeStep = 'scanning';
				await this.discover();
				if (!live()) return;
				if (this.discoverError) throw new Error(this.discoverError);
			}
			this.makeStep = 'planning';
			await this.propose();
			if (!live()) return;
			if (this.proposeError) throw new Error(this.proposeError);
			const track = this.effectiveTrack;
			if (track && this.proposeResult) {
				this.makeStep = 'scoring';
				await api.applyFeedback(this.proposeResult.config_path, {
					kind: 'different_music',
					path: track.path
				});
				if (!live()) return;
				await this.refreshConfig();
				if (!live()) return;
			}
			this.startRenderAsync();
		} catch (e) {
			if (!live()) return;
			this.makeStep = 'failed';
			this.stopBed();
			this.makeError = e instanceof api.ApiError ? e.describe() : e instanceof Error ? e.message : String(e);
		} finally {
			landed();
		}
	}

	/** Bumped to abandon the `makeVideo` run in flight (see `stopRender`). */
	private makeRun = 0;
	/** Settles once the latest `makeVideo` run's server calls are done. */
	private makeInFlight: Promise<void> | null = null;

	/** Reading clips / planning / matching the music: before the first
	 *  render of a project, when there is no server job to stop yet. */
	get preparing(): boolean {
		return (
			!this.rendering &&
			this.renderHistory.length === 0 &&
			(this.makeStep === 'scanning' || this.makeStep === 'planning' || this.makeStep === 'scoring')
		);
	}

	// ---------- Build bed: the song under the wait ----------
	/** Play music while the first video is being cut. */
	musicOn = $state(true);
	/** The finished reel should start playing when it appears. */
	autoplayReveal = $state(false);
	private bed: HTMLAudioElement | null = null;
	private bedFade: ReturnType<typeof setInterval> | null = null;

	private startBed(track: api.Track | null) {
		this.stopBed(true);
		if (!track || !this.musicOn || typeof Audio === 'undefined') return;
		const a = new Audio(api.musicFileUrl(track.path));
		const [from, to] = track.loud_region;
		a.volume = 0;
		a.currentTime = Math.max(0, from);
		// Loop the loud section, so the bed never drifts into a quiet outro.
		a.ontimeupdate = () => {
			if (to > from + 4 && a.currentTime >= to) a.currentTime = from;
		};
		a.onended = () => {
			a.currentTime = Math.max(0, from);
			void a.play().catch(() => {});
		};
		this.bed = a;
		this.autoplayReveal = true;
		void a.play().catch(() => {
			/* blocked: the wait is silent, nothing else changes */
		});
		this.fadeBed(0.22, 900);
	}

	private fadeBed(target: number, ms: number, then?: () => void) {
		if (this.bedFade) clearInterval(this.bedFade);
		const a = this.bed;
		if (!a) return then?.();
		const start = a.volume;
		const t0 = Date.now();
		this.bedFade = setInterval(() => {
			const k = Math.min(1, (Date.now() - t0) / ms);
			a.volume = Math.max(0, Math.min(1, start + (target - start) * k));
			if (k >= 1) {
				if (this.bedFade) clearInterval(this.bedFade);
				this.bedFade = null;
				then?.();
			}
		}, 40);
	}

	stopBed(immediate = false) {
		const a = this.bed;
		if (!a) return;
		const done = () => {
			a.pause();
			if (this.bed === a) this.bed = null;
		};
		if (immediate) {
			if (this.bedFade) clearInterval(this.bedFade);
			this.bedFade = null;
			done();
		} else {
			this.fadeBed(0, 700, done);
		}
	}

	toggleMusic() {
		this.musicOn = !this.musicOn;
		if (!this.musicOn) this.stopBed();
		else if (this.makeStep !== 'done' && this.makeStep !== 'idle') this.startBed(this.effectiveTrack);
	}

	/** Swap the song on the current proposal and re-render. */
	async changeSong(path: string) {
		const title = this.tracks.find((t) => t.path === path)?.title ?? 'new song';
		const before = this.selectedTrackPath;
		await this.withUndo(`Song: ${title}`, async () => {
			this.selectedTrackPath = path;
			if (!this.proposeResult) return;
			await this.quickFeedback({ kind: 'different_music', path });
		});
		// The labels must not claim a song the project didn't take.
		if (this.configError) this.selectedTrackPath = before;
	}

	/** True while a render of the current project is queued or running. */
	get rendering(): boolean {
		return this.renderMode === 'queued' || this.renderMode === 'running';
	}

	/** Apply a quick tweak and immediately re-render (undoable). */
	async tweakAndRender(action: FeedbackAction, label = 'Change') {
		await this.withUndo(label, () => this.quickFeedback(action));
	}

	// ---------- Undo ----------
	/** The last change, undoable until the next one. */
	undo = $state<{
		label: string;
		configPath: string;
		configToml: string;
		propose: api.ProposeResult;
		selectedVibe: string | null;
		selectedTrackPath: string | null;
		versionIndex: number;
	} | null>(null);

	/** Snapshot the project, run `change`, re-render, and offer Undo.
	 *  Undo is instant: the earlier version's file still exists, so it
	 *  only restores the settings and shows that version again.
	 *  `change` may resolve `false`: nothing was changed, so no render. */
	async withUndo(label: string, change: () => Promise<void | boolean>, opts: { render?: boolean } = {}) {
		// Never change the project mid-render: the change would be written
		// but the render guard would refuse to render it.
		if (opts.render !== false && this.rendering) return;
		const prev = this.proposeResult;
		const versionIndex = this.renderHistory.findIndex((v) => v.output_path === this.outputPath);
		const snapshot = prev
			? {
					label,
					configPath: prev.config_path,
					configToml: this.configToml,
					propose: prev,
					selectedVibe: this.selectedVibe,
					selectedTrackPath: this.selectedTrackPath,
					versionIndex
				}
			: null;
		const changed = await change();
		if (changed === false || this.configError || this.makeError) return;
		if (opts.render !== false) void this.startRenderAsync();
		this.undo = snapshot && snapshot.versionIndex >= 0 ? snapshot : null;
		// The render just started is this change's: a Stop undoes it.
		this.renderUndo = this.undo;
	}

	async undoLast() {
		const u = this.undo;
		if (!u) return;
		this.undo = null;
		if (this.renderMode === 'queued' || this.renderMode === 'running') {
			// The render of the change being undone must stop server-side too.
			if (this.jobId) void api.cancelJob(this.jobId).catch(() => {});
			this.resetRender();
		}
		try {
			await api.writeConfig(u.configPath, u.configToml);
			this.proposeResult = { ...u.propose, config_toml: u.configToml };
			this.configToml = u.configToml;
			this.configDirty = false;
			this.selectedVibe = u.selectedVibe;
			this.selectedTrackPath = u.selectedTrackPath;
			this.lastChanges = [];
			await this.loadStructuredConfig();
			this.renderMode = 'done';
			this.makeStep = 'done';
			this.renderResult = this.renderResult ?? ({ ok: true, config_path: u.configPath } as api.RenderResult);
			await this.openVersion(u.versionIndex);
			this.persist();
		} catch (e) {
			this.configError = e instanceof api.ApiError ? e.describe() : String(e);
		}
	}

	/** A contrasting vibe with its own best-matched song. */
	async anotherMix() {
		const NEXT: Record<string, string> = {
			family_chronicle: 'kid_reel',
			kid_reel: 'action_highlight',
			action_highlight: 'drone_cinematic',
			drone_cinematic: 'memory_keeper',
			memory_keeper: 'documentary',
			documentary: 'family_chronicle'
		};
		const next = NEXT[this.effectiveVibe ?? ''] ?? 'family_chronicle';
		const name = this.vibes.find((v) => v.key === next)?.name ?? next;
		await this.withUndo(`Mix: ${name}`, async () => {
			this.selectedVibe = next;
			this.selectedTrackPath = null;
			await this.makeVideo({ bed: false });
		}, { render: false });
	}

	private async refreshConfig() {
		if (!this.proposeResult) return;
		const refreshed = await api.readConfig(this.proposeResult.config_path);
		this.configToml = refreshed.config_toml;
		this.configDirty = false;
		await this.loadStructuredConfig();
	}

	// ---------- Sources ----------
	folder = $state('');
	discovering = $state(false);
	discoverResult = $state<api.DiscoverResult | null>(null);
	discoverError = $state<string | null>(null);

	async discover() {
		this.discovering = true;
		this.discoverError = null;
		try {
			const folder = this.folder;
			this.discoverResult = await api.discoverSources(folder);
			this.discoveredFolder = folder;
			void this.loadSourceThumbs();
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
	/** 'auto' (default), '' (the vibe's own default) or a duration like '30s'. */
	targetDuration = $state('auto');

	/** Vibe picked from the footage when the user hasn't chosen one:
	 *  no sound anywhere reads as drone footage, high-frame-rate clips
	 *  as action, everything else as a family day. */
	autoVibe = $derived.by((): string | null => {
		const d = this.discoverResult;
		if (!d || d.sources.length === 0) return null;
		if (d.sources.every((x) => !x.has_audio)) return 'drone_cinematic';
		if (d.sources.some((x) => x.fps.some((f) => f >= 100))) return 'action_highlight';
		return 'family_chronicle';
	});

	/** The vibe a render will use: the user's choice, else the auto pick. */
	effectiveVibe = $derived(this.selectedVibe ?? this.autoVibe);

	/** Auto length: about a tenth of the footage, 30–90 s, whole 5 s. */
	autoLengthSecs = $derived.by((): number => {
		const total = this.discoverResult?.total_secs ?? 0;
		return Math.round(Math.min(90, Math.max(30, total * 0.1)) / 5) * 5;
	});

	private resolvedDuration(): string | undefined {
		if (this.targetDuration === 'auto') return `${this.autoLengthSecs}s`;
		return this.targetDuration || undefined;
	}
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
		const vibe = this.effectiveVibe;
		if (!vibe) return;
		this.proposing = true;
		this.proposeError = null;
		try {
			const r = await api.proposeVibe({
				folder: this.folder,
				vibe,
				title: this.title || undefined,
				target_duration: this.resolvedDuration(),
				seed: this.seed
			});
			this.proposeResult = r;
			this.configToml = r.config_toml;
			this.configDirty = false;
			// A fresh proposal invalidates whatever the previous one rendered.
			this.resetRender();
			await this.loadStructuredConfig();
		} catch (e) {
			this.proposeError = e instanceof api.ApiError ? e.describe() : String(e);
		} finally {
			this.proposing = false;
		}
	}

	// ---------- Config editor ----------
	//
	// Two views of the same file: `configToml` (raw text, the original
	// hand-edit path) and `configStructured` (typed, what the Guided
	// tab binds its form controls to directly). `configMode` picks
	// which one is visible; both stay live underneath regardless, so
	// switching tabs never loses whichever one was edited last —
	// `saveConfig`/`saveStructuredConfig` each refresh the *other*
	// view after a successful write.
	configToml = $state('');
	configDirty = $state(false);
	configStructured = $state<Config | null>(null);
	configStructuredError = $state<string | null>(null);
	configMode = $state<'guided' | 'raw'>('guided');
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
			// A hand-edit may have touched fields the Guided tab shows —
			// keep it from displaying a stale copy if the user tabs back.
			await this.loadStructuredConfig();
		} catch (e) {
			this.configError = e instanceof api.ApiError ? e.describe() : String(e);
		} finally {
			this.savingConfig = false;
		}
	}

	/** JSON snapshot taken right after every successful load/save —
	 *  `structuredConfigDirty` diffs the live (two-way-bound) object
	 *  against this rather than instrumenting every individual form
	 *  control with its own change handler. */
	private configStructuredSnapshot: string | null = null;

	structuredConfigDirty = $derived.by((): boolean => {
		if (!this.configStructured || this.configStructuredSnapshot === null) return false;
		return JSON.stringify(this.configStructured) !== this.configStructuredSnapshot;
	});

	async loadStructuredConfig() {
		if (!this.proposeResult) return;
		this.configStructuredError = null;
		try {
			this.configStructured = await api.readConfigStructured(this.proposeResult.config_path);
			this.configStructuredSnapshot = JSON.stringify(this.configStructured);
		} catch (e) {
			this.configStructuredError = e instanceof api.ApiError ? e.describe() : String(e);
		}
	}

	async saveStructuredConfig() {
		if (!this.proposeResult || !this.configStructured) return;
		this.savingConfig = true;
		this.configStructuredError = null;
		try {
			const r = await api.writeConfigStructured(this.proposeResult.config_path, this.configStructured);
			this.configStructured = r;
			this.configStructuredSnapshot = JSON.stringify(r);
			this.configChangedSinceRender = true;
			// Keep the Raw tab's textarea in sync too, same reasoning in
			// the other direction as saveConfig()'s reload above.
			const refreshed = await api.readConfig(this.proposeResult.config_path);
			this.configToml = refreshed.config_toml;
			this.configDirty = false;
		} catch (e) {
			this.configStructuredError = e instanceof api.ApiError ? e.describe() : String(e);
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
			await this.loadStructuredConfig();
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
		const old = this.ws;
		this.ws = null;
		old?.close();
		this.renderMode = 'idle';
		this.jobId = null;
		this.renderResult = null;
		this.renderError = null;
		this.frames = [];
	}

	async startRenderAsync() {
		// One render at a time: a second one would share the workspace and
		// the output file with the first.
		if (!this.proposeResult || this.rendering) return;
		// Not a change's render until `withUndo` says so (set synchronously,
		// before the first await, so `withUndo` marks it right after).
		this.renderUndo = null;
		// Render what the user sees: unsaved edits in the Pro settings
		// would otherwise be silently ignored by this render.
		if (this.structuredConfigDirty) await this.saveStructuredConfig();
		else if (this.configDirty) await this.saveConfig();
		const old = this.ws;
		this.ws = null;
		old?.close();
		// Where a Stop goes back to (the version on screen right now).
		this.versionBeforeRender = this.renderHistory.findIndex((v) => v.output_path === this.outputPath);
		this.renderMode = 'queued';
		this.jobId = null; // a stale poll of the previous job must stop
		this.renderError = null;
		this.renderResult = null;
		this.frames = [];
		this.manifest = null;
		this.manifestError = null;
		this.selectedSegmentIndex = null;
		this.configChangedSinceRender = false;
		this.renderProgress = null;
		this.renderStartedAt = Date.now();
		this.makeStep = 'rendering';
		this.jobIdReady = api
			.renderAsync(this.proposeResult.config_path)
			.then((r) => {
				this.jobId = r.job_id;
				this.openProgressSocket(r.job_id);
				return r.job_id;
			})
			.catch((e) => {
				this.renderMode = 'failed';
				this.makeStep = 'failed';
				this.renderError = e instanceof api.ApiError ? e.describe() : String(e);
				return null;
			});
	}

	// ---------- Stop ----------
	/** A Stop is on its way: the button says so and tweaks wait. */
	stopping = $state(false);
	/** Short message for the toast area ("Render stopped — …"). */
	notice = $state<{ text: string; id: number } | null>(null);
	/** The version on screen when the current render started (-1: none). */
	private versionBeforeRender = -1;
	/** The current render's job id, once the server has answered. */
	private jobIdReady: Promise<string | null> | null = null;
	/** The undo snapshot of the change the current render is for, if any. */
	private renderUndo: object | null = null;

	showNotice(text: string) {
		this.notice = { text, id: Date.now() };
	}

	/**
	 * Stop the render in flight and go back to where the user was: the
	 * version on screen before (with the change that started this render
	 * undone, when there is one), or Create for a first render. Resolves
	 * true once stopped, false if there was nothing to stop or the render
	 * finished before the Stop reached it (its result is then shown).
	 * `quiet`: no toast — for "Stop and apply this instead".
	 */
	async stopRender(opts: { quiet?: boolean } = {}): Promise<boolean> {
		if (this.stopping) return false;
		if (!this.rendering) {
			// Still reading the clips or planning: no server job yet, so
			// drop the remaining steps and go back to Create.
			if (!this.preparing) return false;
			this.makeRun++;
			this.makeStep = 'idle';
			this.stopBed();
			this.autoplayReveal = false;
			this.pendingChange = null;
			if (!opts.quiet) this.showNotice('Render stopped');
			return true;
		}
		this.stopping = true;
		try {
			const jobId = this.jobId ?? (await this.jobIdReady);
			if (!jobId || !this.rendering) return false;
			let st: api.JobStatus = await api.cancelJob(jobId);
			const t0 = Date.now();
			while (st.state === 'queued' || st.state === 'running') {
				if (Date.now() - t0 > 20000) throw new Error('it is still running');
				await new Promise((r) => setTimeout(r, 150));
				st = await api.renderStatus(jobId);
			}
			if (this.jobId !== jobId) return false; // superseded meanwhile
			if (st.state !== 'cancelled') {
				// Finished before the Stop landed: show what it made.
				this.applyJobStatus(st);
				return false;
			}
			await this.returnFromStop(opts.quiet);
			return true;
		} catch (e) {
			const why = e instanceof api.ApiError ? e.describe() : e instanceof Error ? e.message : String(e);
			this.showNotice(`Couldn't stop the render: ${why}`);
			return false;
		} finally {
			this.stopping = false;
		}
	}

	/** After a render was cancelled: back to the previous version, or Create. */
	private async returnFromStop(quiet = false) {
		const old = this.ws;
		this.ws = null;
		old?.close();
		this.jobId = null;
		this.renderProgress = null;
		this.renderMode = 'idle';
		this.stopBed();
		this.autoplayReveal = false;
		const u = this.undo;
		let back: number | null = null;
		if (u && u === this.renderUndo && u.versionIndex >= 0 && u.versionIndex < this.renderHistory.length) {
			// The stopped render was for this change: undo it, so the
			// settings match the version shown again.
			back = u.versionIndex;
			await this.undoLast();
			// Couldn't restore the settings (the error says why): still show it.
			if ((this.renderMode as RenderMode) !== 'done') await this.openVersion(back);
		} else if (this.renderHistory.length > 0) {
			const i = this.versionBeforeRender;
			back = i >= 0 && i < this.renderHistory.length ? i : this.renderHistory.length - 1;
			await this.openVersion(back);
		} else {
			this.makeStep = 'idle';
		}
		this.persist();
		if (!quiet) {
			// A plain Stop: the user chose not to apply a pending request.
			this.pendingChange = null;
			const label = back === null ? null : this.renderHistory[back]?.label;
			this.showNotice(label ? `Render stopped — back to ${label}` : 'Render stopped');
		}
	}

	// ---------- Stop and apply this instead ----------
	/** A change asked for while a render runs, waiting on "Stop and apply
	 *  this instead". `from`: the control showing the offer — the command
	 *  bar shows its own; the tweak panel shows the rest (tweak buttons,
	 *  keyboard shortcuts, song picks). */
	pendingChange = $state.raw<{ label: string; from: 'tweaks' | 'command'; run: () => unknown } | null>(null);

	/** Run a change now, or — mid-render — offer to stop the render and
	 *  run it instead. */
	requestChange(label: string, run: () => unknown, from: 'tweaks' | 'command' = 'tweaks') {
		if (this.stopping) return;
		if (this.rendering) {
			this.pendingChange = { label, from, run };
			return;
		}
		this.pendingChange = null;
		void run();
	}

	/** The offer's button: stop the render if it's still going (its own
	 *  change is undone), then run the pending change. */
	async applyPendingChange() {
		const p = this.pendingChange;
		if (!p) return;
		if (this.rendering) {
			await this.stopRender({ quiet: true });
			if (this.rendering) return; // couldn't stop; the toast says why
		}
		if (this.pendingChange !== p) return; // replaced meanwhile
		this.pendingChange = null;
		await p.run();
	}

	// ---------- Renders list ----------
	jobs = $state<api.JobListEntry[]>([]);
	jobsError = $state<string | null>(null);
	loadingJobs = $state(false);

	async loadJobs() {
		this.loadingJobs = true;
		this.jobsError = null;
		try {
			this.jobs = (await api.listJobs()).jobs;
		} catch (e) {
			this.jobsError = e instanceof api.ApiError ? e.describe() : String(e);
		} finally {
			this.loadingJobs = false;
		}
	}

	/** Stop any listed job — the current render goes through `stopRender`. */
	async stopJob(jobId: string) {
		if (jobId === this.jobId) await this.stopRender();
		else {
			try {
				await api.cancelJob(jobId);
			} catch (e) {
				this.jobsError = e instanceof api.ApiError ? e.describe() : String(e);
			}
		}
		await this.loadJobs();
	}

	/** Show a finished job of this project in the Studio. False when it
	 *  belongs to another project (the list links its file instead). */
	async openJob(j: api.JobListEntry): Promise<boolean> {
		if (j.state !== 'done' || this.rendering) return false;
		const path = j.version_path ?? j.output_path;
		if (!path) return false;
		let i = this.renderHistory.findIndex((v) => v.output_path === path);
		if (i < 0 && this.proposeResult && j.config_path === this.proposeResult.config_path) {
			this.recordRenderHistory(j);
			i = this.renderHistory.length - 1;
		}
		if (i < 0) return false;
		await this.openVersion(i);
		this.persist();
		return true;
	}

	/** Apply one job status (from the socket or a poll). */
	private applyJobStatus(data: api.JobStatus | { error: string }) {
		// The unknown-job-id sentinel is the only shape here with no
		// `state` field — narrow on that rather than on `'error' in
		// data`, which TS can't disambiguate cleanly since the `done`
		// variant also carries an *optional* `error` from `RenderResult`.
		if (!('state' in data)) {
			this.renderMode = 'failed';
			this.makeStep = 'failed';
			this.renderError = data.error;
			return;
		}
		if (this.renderMode === 'done' || this.renderMode === 'failed') return;
		if (this.jobId && data.job_id !== this.jobId) return; // an older job
		if (data.state === 'cancelled') {
			// `stopRender` handles its own Stop; this one came from elsewhere
			// (the Renders list, another tab, an MCP client).
			if (!this.stopping && this.rendering) void this.returnFromStop();
			return;
		}
		this.renderMode = data.state;
		if (data.state === 'running') {
			this.renderProgress = data.progress ?? this.renderProgress;
		} else if (data.state === 'done') {
			this.renderResult = data;
			this.makeStep = 'done';
			this.stopBed();
			this.recordRenderHistory(data);
			void this.loadFrames();
			void this.loadManifest();
			this.persist();
		} else if (data.state === 'failed') {
			this.makeStep = 'failed';
			this.stopBed();
			this.autoplayReveal = false;
			this.renderError = data.error;
		}
	}

	private openProgressSocket(jobId: string) {
		const ws = api.renderProgressSocket(jobId);
		this.ws = ws;
		ws.onmessage = (ev) => this.applyJobStatus(JSON.parse(ev.data));
		// A dropped socket (proxy restart, laptop sleep) must not leave the
		// UI on "rendering…" forever: the job keeps running server-side,
		// so fall back to polling its status until it finishes.
		const fallback = () => {
			if (this.ws !== ws) return; // superseded by a newer render
			this.ws = null;
			void this.pollJob(jobId);
		};
		ws.onclose = fallback;
		ws.onerror = fallback;
	}

	private async pollJob(jobId: string) {
		while (this.jobId === jobId && (this.renderMode === 'queued' || this.renderMode === 'running')) {
			try {
				this.applyJobStatus(await api.renderStatus(jobId));
			} catch (e) {
				if (e instanceof api.ApiError && e.status === 404) {
					this.applyJobStatus({ error: 'The server lost track of this render (was it restarted?)' });
					return;
				}
			}
			await new Promise((r) => setTimeout(r, 1500));
		}
	}

	/** The rendered video to show: this render's own version when the
	 *  server kept one, else the configured output path. */
	outputPath = $derived(this.renderResult?.version_path ?? this.renderResult?.output_path ?? null);
	manifestPath = $derived(
		this.renderResult?.version_manifest_path ?? this.renderResult?.manifest_path ?? null
	);

	// ---------- Preview ----------
	frames = $state<api.FrameInline[]>([]);
	loadingFrames = $state(false);
	frameError = $state<string | null>(null);

	async loadFrames() {
		const path = this.outputPath;
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
		const path = this.manifestPath;
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
	 * Segments laid out along the OUTPUT timeline. Exact when the
	 * manifest has a `timeline` (each clip's visible cut in/out, as the
	 * concat stage built it). Older manifests fall back to the beat-sync
	 * cut list, else to cumulative `duration_secs` — the same estimate
	 * `apply_drop_segment` makes server-side for manifests without a
	 * timeline, so feedback identifies the same segment either way.
	 */
	segmentLayout = $derived.by((): SegmentLayout[] => {
		if (!this.manifest) return [];
		const segs = this.manifest.segments;
		let cum = 0;
		const approx = segs.map((s) => {
			const start = cum;
			cum += s.duration_secs;
			return [start, cum] as const;
		});
		// Exact layout: the beat-sync report lists every cut where the
		// concat stage actually put it (crossfade midpoints, intro card
		// included). Clip→clip cuts separate consecutive segments; the
		// burst's internal cuts sit inside one (folded) segment.
		const cuts = this.manifest.beat_sync?.cuts ?? [];
		const body = cuts.filter((c) => c.kind === 'body').map((c) => c.output_secs);
		const intro = cuts.find((c) => c.kind === 'intro_to_body')?.output_secs;
		const outro = cuts.find((c) => c.kind === 'body_to_outro')?.output_secs;
		const exact = body.length === segs.length - 1;
		const fromTimeline = timelineSpans(this.manifest, segs.length);
		const starts = fromTimeline
			? fromTimeline.map((t) => t[0])
			: exact
				? [intro ?? 0, ...body]
				: approx.map((a) => a[0]);
		const ends = fromTimeline
			? fromTimeline.map((t) => t[1])
			: exact
				? [...body, outro ?? this.videoDuration ?? cum]
				: approx.map((a) => a[1]);
		return segs.map((segment, index) => ({
			index,
			segment,
			startSecs: starts[index],
			endSecs: ends[index],
			approxStartSecs: approx[index][0],
			approxEndSecs: approx[index][1]
		}));
	});

	/** True when segment times come from the exact timeline or cut list. */
	exactTimeline = $derived(
		(!!this.manifest && timelineSpans(this.manifest, this.manifest.segments.length) !== null) ||
			(!!this.manifest?.beat_sync &&
				this.manifest.beat_sync.cuts.filter((c) => c.kind === 'body').length ===
					this.manifest.segments.length - 1)
	);

	/** Rendered video length, set by the player once metadata loads. */
	videoDuration = $state<number | null>(null);

	/** Beat times across the rendered video (from the beat-sync report:
	 *  one beat anchor plus the interval — the grid runs unbroken since
	 *  loop crossfades are whole beats). */
	beatTimes = $derived.by((): number[] => {
		const r = this.manifest?.beat_sync;
		const dur =
			this.videoDuration ?? this.manifest?.timeline?.duration_secs ?? this.segmentLayout.at(-1)?.endSecs ?? 0;
		if (!r || r.beat_interval_secs <= 0 || !r.cuts.length || dur <= 0) return [];
		const anchor = r.cuts[0].beat_secs;
		const step = r.beat_interval_secs;
		const first = anchor - Math.floor(anchor / step) * step;
		const out: number[] = [];
		for (let t = first; t < dur && out.length < 4000; t += step) out.push(t);
		return out;
	});

	segmentAt(timeSecs: number): SegmentLayout | null {
		return (
			this.segmentLayout.find((s) => timeSecs >= s.startSecs && timeSecs < s.endSecs) ?? null
		);
	}

	async dropSegment(layout: SegmentLayout) {
		// The server looks the time up in the manifest's timeline when it
		// has one; for older manifests it sums durations, so send that.
		const midpoint =
			this.manifest && timelineSpans(this.manifest, this.manifest.segments.length)
				? (layout.startSecs + layout.endSecs) / 2
				: (layout.approxStartSecs + layout.approxEndSecs) / 2;
		await this.quickFeedback({
			kind: 'drop_segment',
			at_seconds: midpoint,
			// The manifest the timeline was built from (the version shown).
			manifest_path: this.manifestPath ?? this.renderResult?.manifest_path ?? null
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

	/** A Studio request the AI turns into a tweak, as one undoable change:
	 *  it re-renders through `withUndo`, so a Stop (or Undo) puts it back
	 *  like any other tweak instead of leaving it in the next render. */
	async submitBriefChange(label: string) {
		await this.withUndo(label, async () => {
			if (!this.briefText.trim() || this.briefBusy) return false;
			await this.submitBrief();
			return !this.briefError;
		});
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

	private recordRenderHistory(r: api.RenderResult) {
		const outputPath = r.version_path ?? r.output_path;
		if (!outputPath) return;
		const beat = r.manifest_summary?.beat_sync;
		const entry: RenderHistoryEntry = {
			label: `Version ${this.renderHistory.length + 1}`,
			output_path: outputPath,
			manifest_path: r.version_manifest_path ?? r.manifest_path ?? null,
			ts: Date.now(),
			vibe: this.proposeResult?.vibe_name ?? null,
			song: this.effectiveTrack?.title ?? null,
			duration_secs: r.manifest_summary?.actual_duration_secs ?? null,
			beats_locked:
				beat && beat.n_cuts ? `${beat.within_tolerance ?? 0}/${beat.n_cuts} on the beat` : null
		};
		this.renderHistory = [...this.renderHistory, entry];
		this.compareB = this.renderHistory.length - 1;
		this.compareA = this.renderHistory.length >= 2 ? this.renderHistory.length - 2 : null;
	}

	/** Show an earlier version in the player. */
	async openVersion(i: number) {
		const v = this.renderHistory[i];
		if (!v || this.rendering) return;
		// After a failed re-render (or a restore) there is no current
		// result: rebuild one from the history entry so the version can
		// be watched, downloaded and tweaked again.
		const base =
			this.renderResult ??
			({
				ok: true,
				config_path: this.proposeResult?.config_path ?? '',
				output_path: v.output_path,
				manifest_path: v.manifest_path ?? undefined
			} as api.RenderResult);
		this.renderResult = {
			...base,
			version_path: v.output_path,
			version_manifest_path: v.manifest_path ?? undefined
		};
		this.renderMode = 'done';
		this.makeStep = 'done';
		this.renderError = null;
		this.manifest = null; // the timeline waits for this version's own manifest
		this.selectedSegmentIndex = null;
		this.videoDuration = null;
		await Promise.all([this.loadManifest(), this.loadFrames()]);
	}

	// ---------- Persistence ----------

	/** Save what's needed to come back to this session after a reload. */
	persist() {
		const p: Persisted = {
			v: 1,
			folder: this.folder,
			selectedVibe: this.selectedVibe,
			selectedTrackPath: this.selectedTrackPath,
			proposeResult: this.proposeResult,
			renderResult: this.renderMode === 'done' ? this.renderResult : null,
			renderHistory: this.renderHistory
		};
		try {
			localStorage.setItem(STORAGE_KEY, JSON.stringify(p));
		} catch {
			/* private mode / storage full: the session just won't survive a reload */
		}
	}

	/** Restore the last session, re-reading everything from the server
	 *  (a stale entry whose files are gone is dropped quietly). */
	async restore() {
		let p: Persisted | null = null;
		try {
			const raw = localStorage.getItem(STORAGE_KEY);
			p = raw ? (JSON.parse(raw) as Persisted) : null;
		} catch {
			p = null;
		}
		if (!p || p.v !== 1) return;
		this.folder = p.folder;
		this.selectedVibe = p.selectedVibe;
		this.selectedTrackPath = p.selectedTrackPath;
		this.renderHistory = p.renderHistory ?? [];
		if (p.folder) void this.discover();
		if (p.proposeResult) {
			try {
				const cfg = await api.readConfig(p.proposeResult.config_path);
				this.proposeResult = { ...p.proposeResult, config_toml: cfg.config_toml };
				this.configToml = cfg.config_toml;
				await this.loadStructuredConfig();
			} catch {
				// The project is gone: start fresh instead of a dead Studio.
				this.proposeResult = null;
				this.forgetHistory();
				return;
			}
		}
		if (p.renderResult && this.proposeResult) {
			this.renderResult = p.renderResult;
			this.renderMode = 'done';
			this.makeStep = 'done';
			await this.loadManifest();
			if (this.manifestError) {
				this.renderResult = null;
				this.renderMode = 'idle';
				this.makeStep = 'idle';
				this.manifestError = null;
				this.forgetHistory();
			} else {
				void this.loadFrames();
			}
		}
	}

	private forgetHistory() {
		this.renderHistory = [];
		this.compareA = null;
		this.compareB = null;
		this.undo = null;
	}

	/** Forget the current project and go back to the start. */
	startOver() {
		this.resetRender();
		this.proposeResult = null;
		this.configToml = '';
		this.configStructured = null;
		this.makeStep = 'idle';
		this.makeError = null;
		this.renderHistory = [];
		this.lastChanges = [];
		this.persist();
	}
}

/** Run `fn` over `items` with at most `limit` in flight. */
async function runLimited<T>(items: T[], limit: number, fn: (item: T) => Promise<void>) {
	const queue = [...items];
	const workers = Array.from({ length: Math.min(limit, queue.length) }, async () => {
		for (let item = queue.shift(); item !== undefined; item = queue.shift()) {
			await fn(item);
		}
	});
	await Promise.all(workers);
}

export const appState = new AppState();
