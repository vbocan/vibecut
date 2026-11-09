// How each vibe looks and talks in the UI. The server's vibe catalogue
// carries engineering rationale ("energy-arc segment ordering…"); these
// are the human words and the visual identity for the picker.

export interface VibeLook {
	tagline: string;
	/** Accent colours: the card's band and its cut strip. */
	colors: [string, string, string];
	/** What it's best for, in a few words. */
	best: string[];
	/** The card's pictogram (see VibeIcon). */
	icon: 'kids' | 'family' | 'drone' | 'action' | 'talk' | 'memory';
	/** How the edit moves, in words ("Fast cuts on every beat"). */
	pace: string;
	/** A sketch of the cut rhythm: relative shot lengths, left to right. */
	cuts: number[];
	/** How the sound is mixed ("Music up front"). */
	sound: string;
}

export const VIBE_LOOKS: Record<string, VibeLook> = {
	kid_reel: {
		tagline: 'Fast, funny and full of whip-ups.',
		colors: ['#ffb627', '#ff7a45', '#ff3d81'],
		best: ['Kids', 'Playground', 'Birthdays'],
		icon: 'kids',
		pace: 'Quick cuts on the beat',
		cuts: [3, 2, 4, 2, 3, 1.5, 2, 3, 2, 4, 2],
		sound: 'Music, dips for voices'
	},
	family_chronicle: {
		tagline: 'The whole day, in order, voices kept.',
		colors: ['#ffcf99', '#ff8f6b', '#c2537a'],
		best: ['Trips', 'Holidays', 'Gatherings'],
		icon: 'family',
		pace: 'Steady cuts, in time order',
		cuts: [6, 5, 7, 5, 6, 6],
		sound: 'Music dips when people talk'
	},
	drone_cinematic: {
		tagline: 'Long, slow and sweeping. Let the view breathe.',
		colors: ['#38d6ff', '#2a6cf6', '#14204a'],
		best: ['Drone', 'Landscapes', 'Sunsets'],
		icon: 'drone',
		pace: 'Long, sweeping shots',
		cuts: [14, 11, 16],
		sound: 'Music only'
	},
	action_highlight: {
		tagline: 'Punchy cuts on every beat. Pure adrenaline.',
		colors: ['#ff3d81', '#ff5e3a', '#7b5cff'],
		best: ['Bike', 'Ski', 'Surf'],
		icon: 'action',
		pace: 'Fast cuts on every beat',
		cuts: [2, 2, 1.5, 2, 1, 2, 2, 1.5, 2, 1, 2, 2, 1.5, 2],
		sound: 'Music up front'
	},
	documentary: {
		tagline: 'Voices first. The music steps back when people talk.',
		colors: ['#d8c3a5', '#a3a19c', '#5b6478'],
		best: ['Interviews', 'Stories', 'Tours'],
		icon: 'talk',
		pace: 'Long takes for talking',
		cuts: [10, 8, 12, 9],
		sound: 'Voices first, music dips'
	},
	memory_keeper: {
		tagline: 'Long, gentle and soundtrack-led. Made to rewatch.',
		colors: ['#c3a6ff', '#ff9ad5', '#ffcf99'],
		best: ['Vacations', 'Year recaps', 'Weddings'],
		icon: 'memory',
		pace: 'Long, gentle shots',
		cuts: [9, 11, 10, 12],
		sound: 'Music led'
	}
};

const FALLBACK: VibeLook = {
	tagline: 'A fresh take on your footage.',
	colors: ['#ff7a45', '#ff3d81', '#7b5cff'],
	best: [],
	icon: 'memory',
	pace: 'Cuts to the music',
	cuts: [5, 5, 5, 5],
	sound: 'Music and voices'
};

export function vibeLook(key: string): VibeLook {
	return VIBE_LOOKS[key] ?? FALLBACK;
}

/** "3m" / "90s" / "1m30s" → "3 min" / "90 s" / "1 min 30 s". */
export function prettyDuration(d: string): string {
	const m = /^(?:(\d+)m)?(?:(\d+)s)?$/.exec(d.trim());
	if (!m || (!m[1] && !m[2])) return d;
	const parts = [];
	if (m[1]) parts.push(`${m[1]} min`);
	if (m[2]) parts.push(`${m[2]} s`);
	return parts.join(' ');
}

/** Seconds → "1:07" (or "1:07.4" with `tenths`). */
export function clock(t: number, tenths = false): string {
	if (!Number.isFinite(t) || t < 0) t = 0;
	const m = Math.floor(t / 60);
	const s = t - m * 60;
	const ss = tenths ? s.toFixed(1).padStart(4, '0') : Math.floor(s).toString().padStart(2, '0');
	return `${m}:${ss}`;
}

/** Bytes → "1.2 GB". */
export function prettyBytes(n: number): string {
	const units = ['B', 'KB', 'MB', 'GB', 'TB'];
	let i = 0;
	while (n >= 1024 && i < units.length - 1) {
		n /= 1024;
		i++;
	}
	return `${n.toFixed(n >= 10 || i === 0 ? 0 : 1)} ${units[i]}`;
}
