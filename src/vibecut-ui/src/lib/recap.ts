// "Your year" recap: what the Create screen's recap card shows. The server
// plans the recap (GET /api/recap); these helpers only decide which year
// to offer and how its months or trips read.
//
// Pure functions, type-only imports: `node --test src/lib/recap.test.mjs`
// runs their tests without a browser.

import type { ProposeResult, RecapArgs, RecapPlan, RecapPreview, RecapResult } from './api';
import type { SelectionExclude } from './types/generated/SelectionExclude';
import type { Transition } from './types/generated/Transition';

/** Lengths the card offers, seconds. */
export const RECAP_LENGTHS: readonly number[] = [45, 60, 90];

const MONTHS = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec'];

/** The year the card offers: the latest one with enough months or trips. */
export function offeredYear(p: RecapPreview | null): number | null {
	return p?.years.find((y) => y.eligible)?.year ?? null;
}

/** One month (or trip) on the card. */
export interface RecapTile {
	key: string;
	label: string;
	/** Second line: the trips of a month, or the day a trip started. */
	detail: string;
	/** Moments the recap plays from it (0: left out). */
	chosen: number;
	leftOut: boolean;
	/** Its first moment in the recap, mid-clip: what the thumbnail shows. */
	thumb: { path: string; t: number } | null;
}

/** "Mar 14" from an RFC 3339 time (UTC, as the camera wrote it). */
export function shortDay(iso: string): string {
	const m = /^(\d{4})-(\d{2})-(\d{2})/.exec(iso);
	if (!m) return '';
	return `${MONTHS[Number(m[2]) - 1] ?? ''} ${Number(m[3])}`.trim();
}

/** The card's tiles, in the recap's order. */
export function recapTiles(plan: RecapPlan): RecapTile[] {
	return plan.buckets.map((b) => {
		const first = plan.moments.find((m) => m.bucket === b.label);
		const detail =
			b.chosen === 0
				? 'No room at this length'
				: plan.grouping === 'month'
					? listOf(b.trips, 2)
					: shortDay(b.first_recorded);
		return {
			key: b.key,
			label: b.label,
			detail,
			chosen: b.chosen,
			leftOut: b.chosen === 0,
			thumb: first ? { path: first.path, t: (first.clip_start_secs + first.clip_end_secs) / 2 } : null
		};
	});
}

/** "Alps, Coast", "Alps, Coast +2". */
function listOf(items: string[], max: number): string {
	const shown = items.slice(0, max).join(', ');
	return items.length > max ? `${shown} +${items.length - max}` : shown;
}

/** Seconds → "1:05". */
export function clock(secs: number): string {
	const s = Math.max(0, Math.round(secs));
	return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, '0')}`;
}

/** "15 moments · 5 months · about 1:00". */
export function recapSummary(plan: RecapPlan): string {
	const n = plan.moments.length;
	const used = plan.buckets.filter((b) => b.chosen > 0).length;
	const unit = plan.grouping === 'month' ? 'month' : 'trip';
	return `${n} moment${n === 1 ? '' : 's'} · ${used} ${unit}${used === 1 ? '' : 's'} · about ${clock(plan.estimated_secs)}`;
}

/** Cache key of a tile's thumbnail (the same bucket can show another moment
 *  at another length). */
export function thumbKey(t: RecapTile): string {
	return t.thumb ? `${t.thumb.path}@${t.thumb.t.toFixed(2)}` : t.key;
}

/** The recap as the project the Studio works on: its config, song and
 *  title, and what to make it again from (a new length, another mix). */
export function recapProposal(r: RecapResult): ProposeResult {
	return {
		vibe: r.vibe,
		vibe_name: 'Year recap',
		rationale: r.rationale,
		config_path: r.config_path,
		config_toml: r.config_toml,
		output_path: r.output_path,
		soundtrack: r.soundtrack,
		auto_pick: r.auto_pick,
		music_note: r.music_note,
		title: r.plan.title,
		recap: {
			year: r.plan.period.year,
			from: r.plan.period.from,
			to: r.plan.period.to,
			length: r.plan.length_secs,
			shuffle: r.plan.shuffle ?? null
		}
	};
}

/** What making the recap on screen again asks the server: its period,
 *  length, vibe and shuffle, with `changes` applied. Another mix (a new
 *  `vibe`) lets that vibe pick the song; every other remake keeps the song
 *  playing (`song`: the project's song, null for none, undefined when
 *  unknown). Every remake keeps what the project's config says beyond the
 *  moments (`kept`): the moments the user dropped (its
 *  `[[selection.exclude]]`) and its `[transition]`, once the config is
 *  loaded. Null when `p` is no recap. */
export function recapRemakeArgs(
	p: ProposeResult,
	changes: { length?: number; vibe?: string; shuffle?: number },
	song: string | null | undefined,
	kept: { exclude: SelectionExclude[]; transition?: Transition },
	seed: number
): RecapArgs | null {
	const r = p.recap;
	if (!r) return null;
	const shuffle = changes.shuffle ?? r.shuffle ?? null;
	const keepSong = changes.vibe === undefined && song !== undefined;
	return {
		...(r.year !== null ? { year: r.year } : { from: r.from, to: r.to }),
		length: changes.length ?? r.length,
		vibe: changes.vibe ?? p.vibe,
		...(keepSong ? { soundtrack: song ?? 'none' } : {}),
		...(shuffle !== null ? { shuffle } : {}),
		exclude: kept.exclude,
		...(kept.transition ? { transition: kept.transition } : {}),
		seed,
		render: false
	};
}

/** The length a "Shorter" / "Longer" tweak asks of a recap: scaled,
 *  whole seconds, within what the server accepts (20 s – 5 min). */
export function scaledRecapLength(length: number, factor: number): number {
	return Math.round(Math.min(300, Math.max(20, length * factor)));
}
