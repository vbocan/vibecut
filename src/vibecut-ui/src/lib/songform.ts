// The song's form on the beat timeline: beat ticks, bar lines, phrase
// starts and the loud / quiet sections, from the manifest's `music`
// record (written by the engine's music-led pacing, `vibecut::pacing`).
//
// Pure functions, no imports with side effects: `node --test
// src/lib/songform.test.mjs` runs their tests without a browser.

import type { MusicRecord } from './types/generated/MusicRecord';
import type { SectionRecord } from './types/generated/SectionRecord';
import type { HeroSpot } from './types/generated/HeroSpot';
import type { EndingRecord } from './types/generated/EndingRecord';

export type GridKind = 'beat' | 'bar' | 'phrase';

export interface GridLine {
	t: number;
	kind: GridKind;
}

/** Lines closer than this (seconds) are the same line. */
const SAME_LINE_SECS = 0.03;

function hasNear(sorted: number[], t: number): boolean {
	let lo = 0;
	let hi = sorted.length;
	while (lo < hi) {
		const mid = (lo + hi) >> 1;
		if (sorted[mid] < t - SAME_LINE_SECS) lo = mid + 1;
		else hi = mid;
	}
	return lo < sorted.length && sorted[lo] <= t + SAME_LINE_SECS;
}

/** Every beat, bar line and phrase start under the video, sorted; where
 *  two coincide the heavier kind is kept. Without the song's form (older
 *  manifests) every fourth beat from the first is drawn as a bar. */
export function gridLines(
	beats: number[],
	music: Pick<MusicRecord, 'downbeats' | 'phrases'> | null | undefined
): GridLine[] {
	if (!music) return beats.map((t, i) => ({ t, kind: i % 4 === 0 ? 'bar' : 'beat' }));
	const phrases = [...music.phrases].sort((a, b) => a - b);
	const bars = [...music.downbeats].sort((a, b) => a - b);
	const out: GridLine[] = phrases.map((t) => ({ t, kind: 'phrase' as const }));
	for (const t of bars) if (!hasNear(phrases, t)) out.push({ t, kind: 'bar' });
	for (const t of beats) if (!hasNear(bars, t) && !hasNear(phrases, t)) out.push({ t, kind: 'beat' });
	return out.sort((a, b) => a.t - b.t);
}

/** Canvas fill for a section band: loud parts lit in the brand magenta
 *  (the drop brightest), quiet ones a cool tint, the middle barely. */
export function sectionFill(sec: Pick<SectionRecord, 'level' | 'drop'>): string {
	if (sec.drop) return 'rgba(255, 61, 129, 0.26)';
	switch (sec.level) {
		case 'high':
			return 'rgba(255, 61, 129, 0.13)';
		case 'quiet':
			return 'rgba(56, 214, 255, 0.08)';
		default:
			return 'rgba(255, 255, 255, 0.025)';
	}
}

/** Where the best moment starts, in words. */
export function heroSpotText(on: HeroSpot): string {
	switch (on) {
		case 'drop':
			return 'on the drop';
		case 'section':
			return 'on a loud part';
		default:
			return 'on a phrase';
	}
}

function mmss(t: number): string {
	const s = Math.max(0, Math.round(t));
	return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, '0')}`;
}

function endingText(e: EndingRecord): string {
	const at = mmss(e.last_cut_secs);
	switch (e.kind) {
		case 'song_end':
			return `it ends with the song: the last cut on its final hit at ${at}`;
		case 'phrase_end':
			return `it ends on a phrase start at ${at}`;
		case 'bar_end':
			return `it ends on a bar line at ${at}`;
		default:
			return `it ends on a beat at ${at}`;
	}
}

/** One sentence for screen readers about the song's form and what the
 *  edit made of it; empty without the form. */
export function describeSongForm(music: MusicRecord | null | undefined): string {
	if (!music || music.sections.length === 0) return '';
	const n = music.sections.length;
	const loud = music.sections.filter((s) => s.level === 'high').length;
	const parts = [`${n} section${n === 1 ? '' : 's'}, ${loud} of them loud`];
	const drop = music.sections.find((s) => s.drop);
	if (drop) parts.push(`the drop at ${mmss(drop.start_secs)}`);
	if (music.hero) parts.push(`the best moment starts ${heroSpotText(music.hero.on)} at ${mmss(music.hero.start_secs)}`);
	if (music.ending) parts.push(endingText(music.ending));
	return `Song: ${parts.join('; ')}.`;
}
