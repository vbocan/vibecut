// How each vibe looks and talks in the UI. The server's vibe catalogue
// carries engineering rationale ("energy-arc segment ordering…"); these
// are the human words and the visual identity for the picker.

export interface VibeLook {
	tagline: string;
	/** Three colours for the tile's living gradient. */
	colors: [string, string, string];
	/** Motion personality of the tile animation. */
	motion: 'bounce' | 'drift' | 'glide' | 'strobe' | 'calm' | 'float';
	/** What it's best for, as short chips. */
	best: string[];
}

export const VIBE_LOOKS: Record<string, VibeLook> = {
	kid_reel: {
		tagline: 'Fast, funny and full of whip-ups.',
		colors: ['#ffb627', '#ff7a45', '#ff3d81'],
		motion: 'bounce',
		best: ['Kids', 'Playground', 'Birthdays']
	},
	family_chronicle: {
		tagline: 'The whole day, in order, voices kept.',
		colors: ['#ffcf99', '#ff8f6b', '#c2537a'],
		motion: 'drift',
		best: ['Trips', 'Holidays', 'Gatherings']
	},
	drone_cinematic: {
		tagline: 'Long, slow and sweeping. Let the view breathe.',
		colors: ['#38d6ff', '#2a6cf6', '#14204a'],
		motion: 'glide',
		best: ['Drone', 'Landscapes', 'Sunsets']
	},
	action_highlight: {
		tagline: 'Punchy cuts on every beat. Pure adrenaline.',
		colors: ['#ff3d81', '#ff5e3a', '#7b5cff'],
		motion: 'strobe',
		best: ['Bike', 'Ski', 'Surf']
	},
	documentary: {
		tagline: 'Voices first. The music steps back when people talk.',
		colors: ['#d8c3a5', '#8e8d8a', '#3b4252'],
		motion: 'calm',
		best: ['Interviews', 'Stories', 'Tours']
	},
	memory_keeper: {
		tagline: 'Long, gentle and soundtrack-led. Made to rewatch.',
		colors: ['#c3a6ff', '#ff9ad5', '#ffcf99'],
		motion: 'float',
		best: ['Vacations', 'Year recaps', 'Weddings']
	}
};

const FALLBACK: VibeLook = {
	tagline: 'A fresh take on your footage.',
	colors: ['#ff7a45', '#ff3d81', '#7b5cff'],
	motion: 'drift',
	best: []
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
