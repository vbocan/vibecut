// "Tell it" without an LLM: a small, deterministic grammar for the things
// people actually say about a reel. Used when the server has no API key
// (health.ai = false) and as a fast path before asking the model.

import type { FeedbackAction } from './api';

export type TellResult =
	| { kind: 'action'; action: FeedbackAction; label: string }
	| { kind: 'song'; label: string }
	| { kind: 'mix'; label: string }
	| { kind: 'drop_at'; seconds: number; label: string }
	| { kind: 'source'; more: boolean; sourceId: string; label: string }
	| { kind: 'plan'; vibe: string | null; duration: string | null; label: string }
	| { kind: 'unknown' };

const has = (t: string, re: RegExp) => re.test(t);

/** Seconds from "0:42", "1:05", "42s", "42 sec", "at 42". */
function timeIn(t: string): number | null {
	const mmss = /(\d{1,2}):(\d{2})/.exec(t);
	if (mmss) return Number(mmss[1]) * 60 + Number(mmss[2]);
	const secs = /(\d+(?:\.\d+)?)\s*(?:s|sec|secs|seconds?)\b/.exec(t);
	if (secs) return Number(secs[1]);
	const at = /\bat\s+(\d+(?:\.\d+)?)\b/.exec(t);
	return at ? Number(at[1]) : null;
}

/** A length like "30 seconds", "under a minute", "2 min" → "30s" / "55s" / "2m". */
function lengthIn(t: string): string | null {
	if (has(t, /under (a|one) minute|less than a minute/)) return '55s';
	const m = /(\d+(?:\.\d+)?)\s*(m|min|mins|minutes?)\b/.exec(t);
	if (m) return `${Math.round(Number(m[1]) * 60)}s`;
	const s = /(\d+)\s*(s|sec|secs|seconds?)\b/.exec(t);
	if (s) return `${s[1]}s`;
	if (has(t, /\b(reel|short|tiktok|instagram|shorts)\b/)) return '30s';
	return null;
}

/** Interpret a request about an existing reel. */
export function tellStudio(input: string): TellResult {
	const t = input.toLowerCase().trim();
	if (!t) return { kind: 'unknown' };

	if (has(t, /\b(drop|remove|cut out|delete|lose)\b/)) {
		const secs = timeIn(t);
		if (secs !== null) return { kind: 'drop_at', seconds: secs, label: `Drop the moment at ${fmt(secs)}` };
	}
	const src = /\b(more|less) of\s+(?:clip|camera|source)?\s*([a-z0-9_-]+)/.exec(t);
	if (src) {
		const more = src[1] === 'more';
		return { kind: 'source', more, sourceId: src[2], label: `${more ? 'More' : 'Less'} of clip ${src[2]}` };
	}
	if (has(t, /another mix|different (vibe|style|mix)|surprise me/)) return { kind: 'mix', label: 'Another mix' };
	if (has(t, /(new|different|another|other|change( the)?) (song|music|track)/))
		return { kind: 'song', label: 'New song' };
	if (has(t, /much shorter|half (the )?length|way shorter/))
		return { kind: 'action', action: { kind: 'scale_duration', factor: 0.5 }, label: 'Much shorter' };
	if (has(t, /much longer|twice as long|double/))
		return { kind: 'action', action: { kind: 'scale_duration', factor: 2 }, label: 'Much longer' };
	if (has(t, /\b(shorter|shorten|trim|tighter|too long)\b/))
		return { kind: 'action', action: { kind: 'scale_duration', factor: 0.75 }, label: 'Shorter' };
	if (has(t, /\b(longer|extend|too short)\b/))
		return { kind: 'action', action: { kind: 'scale_duration', factor: 1.25 }, label: 'Longer' };
	if (has(t, /\b(calm|calmer|slower|gentle|gentler|softer|chill|less aggressive|relax)/))
		return { kind: 'action', action: { kind: 'calmer' }, label: 'Calmer' };
	if (has(t, /\b(punch|punchier|energy|faster|hype|intense|aggressive|snappier|exciting)/))
		return { kind: 'action', action: { kind: 'punchier' }, label: 'More energy' };
	if (has(t, /\b(shuffle|reroll|re-roll|different moments|other moments|again|mix it up)\b/))
		return { kind: 'action', action: { kind: 'reroll' }, label: 'Shuffle' };
	return { kind: 'unknown' };
}

const VIBE_WORDS: [RegExp, string][] = [
	[/\b(drone|aerial|cinematic|landscape|sunset|scenic)/, 'drone_cinematic'],
	[/\b(action|bike|biking|ski|skiing|surf|surfing|mtb|adrenaline|sport|extreme|moto)/, 'action_highlight'],
	[/\b(kid|kids|children|playground|birthday|toddler)/, 'kid_reel'],
	[/\b(documentary|interview|voices?|talking|story)/, 'documentary'],
	[/\b(memories|memory|wedding|year|recap|vacation|gentle|long)/, 'memory_keeper'],
	[/\b(family|trip|day out|holiday|weekend|chronicle)/, 'family_chronicle']
];

/** Interpret a request for a new video. */
export function tellCreate(input: string): TellResult {
	const t = input.toLowerCase().trim();
	if (!t) return { kind: 'unknown' };
	const vibe = VIBE_WORDS.find(([re]) => re.test(t))?.[1] ?? null;
	const duration = lengthIn(t);
	if (!vibe && !duration) return { kind: 'unknown' };
	return { kind: 'plan', vibe, duration, label: 'Got it' };
}

function fmt(s: number): string {
	return `${Math.floor(s / 60)}:${Math.floor(s % 60)
		.toString()
		.padStart(2, '0')}`;
}
