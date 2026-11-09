// Pure layout for the storyboard: the planned edit (POST /api/plan) laid
// out on its own beat grid before anything is encoded. Kept free of runes
// and $lib imports so `node --test` can load it.

/** The parts of a plan the storyboard reads (see `api.EditPlan`). */
export interface StoryboardPlan {
	duration_secs: number;
	beats: number[];
	song?: { tempo_bpm: number; beat_interval_secs: number } | null;
	timeline: {
		intro?: { output_start_secs: number; output_end_secs: number; cut_secs: number };
		outro?: { output_start_secs: number; output_end_secs: number; cut_secs: number };
		clips: { segment_index: number; burst_part?: number; cut_in_secs: number; cut_out_secs: number; ramp?: unknown }[];
	};
	thumbnails: { clip_index: number; source_path: string; t_secs: number }[];
}

/** One tile on the strip, as percentages of the video's length. */
export interface StoryboardTile {
	kind: 'intro' | 'clip' | 'outro';
	/** Index into `timeline.clips` (clips only). */
	clip: number | null;
	segment: number | null;
	left: number;
	width: number;
	/** Part of the beat-burst (rapid cuts on every beat). */
	burst: boolean;
	/** Ends in a speed ramp. */
	ramp: boolean;
	startSecs: number;
	endSecs: number;
}

const pct = (t: number, total: number) => (total > 0 ? Math.min(100, Math.max(0, (t / total) * 100)) : 0);

/** Every visible span in output order: the cards and each clip from its cut
 *  in to its cut out (the spans tile the video, crossfades split midway). */
export function storyboardTiles(plan: StoryboardPlan): StoryboardTile[] {
	const total = plan.duration_secs;
	const tiles: StoryboardTile[] = [];
	const span = (kind: StoryboardTile['kind'], a: number, b: number, extra: Partial<StoryboardTile> = {}) =>
		tiles.push({
			kind,
			clip: null,
			segment: null,
			burst: false,
			ramp: false,
			...extra,
			left: pct(a, total),
			width: Math.max(0, pct(b, total) - pct(a, total)),
			startSecs: a,
			endSecs: b
		});
	const { intro, outro, clips } = plan.timeline;
	if (intro) span('intro', 0, intro.cut_secs);
	clips.forEach((c, i) =>
		span('clip', c.cut_in_secs, c.cut_out_secs, {
			clip: i,
			segment: c.segment_index,
			burst: c.burst_part !== undefined && c.burst_part !== null,
			ramp: !!c.ramp
		})
	);
	if (outro) span('outro', outro.cut_secs, total);
	return tiles;
}

/** Beat ticks along the strip; every fourth (from the first beat) is a
 *  bar line. Thinned to at most `max` so a long video stays readable. */
export function beatTicks(plan: StoryboardPlan, max = 160): { left: number; bar: boolean }[] {
	const beats = plan.beats.filter((b) => b >= 0 && b <= plan.duration_secs);
	const step = Math.max(1, Math.ceil(beats.length / max));
	const out: { left: number; bar: boolean }[] = [];
	for (let i = 0; i < beats.length; i += step) out.push({ left: pct(beats[i], plan.duration_secs), bar: i % 4 === 0 });
	return out;
}

/** Frame requests for the thumbnails, grouped by source file (one request
 *  per file, at most `perCall` frames each — the server caps a call): the
 *  clips each request serves and the source times to grab. */
export function thumbRequests(
	plan: StoryboardPlan,
	perCall = 12
): { source: string; clips: number[]; times: number[] }[] {
	const out: { source: string; clips: number[]; times: number[] }[] = [];
	const open = new Map<string, { source: string; clips: number[]; times: number[] }>();
	for (const t of plan.thumbnails) {
		let r = open.get(t.source_path);
		if (!r || r.times.length >= perCall) {
			r = { source: t.source_path, clips: [], times: [] };
			open.set(t.source_path, r);
			out.push(r);
		}
		r.clips.push(t.clip_index);
		r.times.push(t.t_secs);
	}
	return out;
}

/** Pair the frames that came back (`t_secs` each, some may be missing) with
 *  the clips that asked for them: each frame goes to the clip whose time is
 *  nearest (the server rounds the times it was sent). */
export function matchFrames(
	req: { clips: number[]; times: number[] },
	frames: { t_secs: number; data_uri: string }[]
): Record<number, string> {
	const out: Record<number, string> = {};
	for (const f of frames) {
		let best = -1;
		let bestErr = Infinity;
		req.times.forEach((t, k) => {
			const err = Math.abs(t - f.t_secs);
			if (err < bestErr) {
				bestErr = err;
				best = k;
			}
		});
		if (best >= 0 && bestErr < 0.05) {
			// Clips that asked for (nearly) the same moment share the frame.
			req.times.forEach((t, k) => {
				if (Math.abs(t - req.times[best]) < 1e-6) out[req.clips[k]] = f.data_uri;
			});
		}
	}
	return out;
}

/** How many clips are encoded, from a render's progress: all of them past
 *  the clips stage, the count in "7 of 20 clips" during it, none before. */
export function clipsCut(
	progress: { stage: string; detail: string | null } | null,
	nClips: number
): number {
	if (!progress) return 0;
	const after = ['joining', 'mixing', 'encoding', 'checking'];
	if (after.includes(progress.stage)) return nClips;
	if (progress.stage !== 'clips') return 0;
	const m = /^(\d+) of (\d+)/.exec(progress.detail ?? '');
	return m ? Math.min(nClips, Number(m[1])) : 0;
}

/** "20 cuts · 132 BPM · 0:29" */
export function storyboardCaption(plan: StoryboardPlan): string {
	const cuts = plan.timeline.clips.length;
	const secs = Math.round(plan.duration_secs);
	const clock = `${Math.floor(secs / 60)}:${String(secs % 60).padStart(2, '0')}`;
	const bpm = plan.song && plan.song.tempo_bpm > 0 ? ` · ${Math.round(plan.song.tempo_bpm)} BPM` : '';
	return `${cuts} ${cuts === 1 ? 'shot' : 'shots'}${bpm} · ${clock}`;
}
