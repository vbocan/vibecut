// Pure rules for platform versions that change the picture's shape (a
// vertical Reel, a square or 4:5 post from a 16:9 render): which framing
// a version gets, and where a smart ("Follow the action") crop sat, for
// the Share sheet's before/after preview. Kept free of runes and $lib
// imports so `node --test` can load it.

/** How a version of another shape is framed: a crop that follows the
 *  action (`smart`), or the whole frame over a blurred fill (`blur`). */
export type Framing = 'smart' | 'blur';

/** The camera path a smart version followed (the reformat result's
 *  `smart`): the crop window's share of the source frame along `axis`,
 *  and its centre over time as `[t0, t1, c0, c1]` pieces (linear in
 *  between, jumping only at cuts). */
export interface SmartPath {
	axis: 'x' | 'y';
	window: number;
	shots: number;
	followed_shots: number;
	pieces: [number, number, number, number][];
}

/** Share of a smart version's progress bar spent finding the action
 *  (vibecut::output_format::SMART_ANALYSIS_SHARE). */
export const SMART_ANALYSIS_SHARE = 0.25;

/** `"1920x1080"` → 1920 / 1080; null when unreadable. */
export function aspectOf(resolution: string | null | undefined): number | null {
	const m = /^\s*(\d+)\s*[x×]\s*(\d+)\s*$/.exec(resolution ?? '');
	if (!m) return null;
	const [w, h] = [Number(m[1]), Number(m[2])];
	return w > 0 && h > 0 ? w / h : null;
}

/** Whether a `w:h` platform shape reframes a render of `resolution` (the
 *  aspects differ by more than 1 %, like the engine's `same_aspect`). An
 *  unknown render is taken as 16:9, the master shape. */
export function reframes(resolution: string | null | undefined, w: number, h: number): boolean {
	const src = aspectOf(resolution) ?? 16 / 9;
	return Math.abs(w / h / src - 1) > 0.01;
}

/** The `fit` to ask for: the framing when the shape changes, nothing
 *  when it doesn't (a same-shape version has no framing to choose). */
export function fitFor(framing: Framing, reframed: boolean): Framing | undefined {
	return reframed ? framing : undefined;
}

/** Window centre at `t` (0.5 = the centre crop). */
export function centreAt(path: SmartPath, t: number): number {
	const ps = path.pieces;
	if (ps.length === 0) return 0.5;
	const p = ps.find((q) => t < q[1]) ?? ps[ps.length - 1];
	const [t0, t1, c0, c1] = p;
	if (t1 <= t0) return c0;
	const f = Math.min(1, Math.max(0, (t - t0) / (t1 - t0)));
	return c0 + (c1 - c0) * f;
}

/** The crop at `t` as percentages of the source frame, for an overlay
 *  box: `{ left, top, width, height }`. */
export function cropBoxAt(
	path: SmartPath,
	t: number
): { left: number; top: number; width: number; height: number } {
	const w = Math.min(1, Math.max(0, path.window));
	const c = Math.min(1 - w / 2, Math.max(w / 2, centreAt(path, t)));
	const start = (c - w / 2) * 100;
	return path.axis === 'x'
		? { left: start, top: 0, width: w * 100, height: 100 }
		: { left: 0, top: start, width: 100, height: w * 100 };
}

/** A moment that shows what the smart crop did: where it sits furthest
 *  from the centre (sampled every half second, clear of the first and
 *  last second, where titles sit), else the middle. */
export function previewTime(path: SmartPath | null | undefined, duration: number): number {
	const mid = Math.max(0, duration / 2);
	if (!path || !(duration > 2)) return mid;
	let best = mid;
	let off = 0.02;
	for (let t = 1; t <= duration - 1; t += 0.5) {
		const d = Math.abs(centreAt(path, t) - 0.5);
		if (d > off + 1e-9) {
			off = d;
			best = t;
		}
	}
	return best;
}

/** A version's progress line: a smart one first looks for the action. */
export function progressLabel(fit: Framing | undefined, fraction: number, label: string): string {
	const pct = Math.round(Math.min(1, Math.max(0, fraction)) * 100);
	if (fit === 'smart' && fraction < SMART_ANALYSIS_SHARE) return `Finding the action… ${pct}%`;
	return `Making the ${label} version… ${pct}%`;
}
