// The Fine-tune page's controls: each one names a key in the project's
// TOML (dotted, as POST /api/config/patch takes it), how to show it, and
// what it does in plain words. Pure data and helpers: `node --test
// src/lib/finetune.test.mjs` runs their tests.

export type ControlKind = 'range' | 'seconds' | 'choice' | 'toggle' | 'text';

export interface Control {
	key: string;
	label: string;
	/** What changing it does, in a sentence. */
	help: string;
	kind: ControlKind;
	min?: number;
	max?: number;
	step?: number;
	/** How a number reads ("−16 dB", "40%"). */
	unit?: 'db' | 'percent' | 'seconds' | 'plain';
	choices?: { value: string | number; label: string }[];
	/** Only shown when the project has a soundtrack / an intro / an outro. */
	needs?: 'soundtrack' | 'intro' | 'outro';
}

export interface Group {
	id: string;
	label: string;
	blurb: string;
	controls: Control[];
}

const weight = (key: string, label: string, help: string): Control => ({
	key: `scoring.${key}`,
	label,
	help,
	kind: 'range',
	min: 0,
	max: 1,
	step: 0.05,
	unit: 'percent'
});

export const GROUPS: Group[] = [
	{
		id: 'story',
		label: 'Story',
		blurb: 'How long the video is and in what order things happen.',
		controls: [
			{ key: 'project.target_duration', label: 'Length', help: 'How long the finished video is.', kind: 'seconds', min: 10, max: 600, step: 5 },
			{
				key: 'selection.order',
				label: 'Order',
				help: 'In time order tells the day as it happened; building to the best moment puts the strongest shots in the middle.',
				kind: 'choice',
				choices: [
					{ value: 'chronological', label: 'In time order' },
					{ value: 'energy_arc', label: 'Build to the best moment' },
					{ value: 'strict_chronological', label: 'Strictly by the clock' }
				]
			},
			{ key: 'project.fade_in', label: 'Fade in from black', help: 'How long the video takes to appear at the start.', kind: 'seconds', min: 0, max: 3, step: 0.1 },
			{ key: 'project.fade_out', label: 'Fade out to black', help: 'How long the last shot takes to fade away.', kind: 'seconds', min: 0, max: 6, step: 0.1 }
		]
	},
	{
		id: 'rhythm',
		label: 'Rhythm',
		blurb: 'How fast it cuts and how much it changes between renders.',
		controls: [
			{ key: 'selection.min_clip', label: 'Shortest shot', help: 'No shot is shorter than this.', kind: 'seconds', min: 0.5, max: 10, step: 0.5 },
			{ key: 'selection.max_clip', label: 'Longest shot', help: 'No shot is longer than this. Shorter means more cuts.', kind: 'seconds', min: 1, max: 30, step: 0.5 },
			{
				key: 'soundtrack.energy_drives_pacing',
				label: "Follow the music's energy",
				help: 'Quiet parts of the song get longer shots, loud parts faster cuts.',
				kind: 'toggle',
				needs: 'soundtrack'
			},
			{
				key: 'soundtrack.pacing_strength',
				label: 'How strongly',
				help: 'How much the music changes the shot lengths.',
				kind: 'range',
				min: 0,
				max: 1,
				step: 0.05,
				unit: 'percent',
				needs: 'soundtrack'
			},
			{
				key: 'selection.variety',
				label: 'Shake it up between renders',
				help: 'At 0 a render picks the same moments every time; higher picks differently each time.',
				kind: 'range',
				min: 0,
				max: 1,
				step: 0.05,
				unit: 'percent'
			},
			{
				key: 'transition.speed_ramp_probability',
				label: 'Whip-ups',
				help: 'How many cuts get a quick speed-up into the next shot.',
				kind: 'range',
				min: 0,
				max: 1,
				step: 0.05,
				unit: 'percent'
			},
			{ key: 'transition.speed_ramp_flash', label: 'Flash on whip-ups', help: 'A brief flash of light at the end of each whip-up.', kind: 'toggle' }
		]
	},
	{
		id: 'music',
		label: 'Music',
		blurb: 'The song, and how it sits with the sound of your clips.',
		controls: [
			{ key: 'soundtrack.beat_align', label: 'Cut on the beat', help: 'Every cut lands on a beat of the song.', kind: 'toggle', needs: 'soundtrack' },
			{
				key: 'soundtrack.ducking_db',
				label: 'Music dips for voices by',
				help: 'How much quieter the music gets when your clips have something worth hearing. 0 dB never dips.',
				kind: 'range',
				min: -30,
				max: 0,
				step: 1,
				unit: 'db',
				needs: 'soundtrack'
			},
			{ key: 'soundtrack.duck_count', label: 'How many dips', help: 'The most moments where the music steps back.', kind: 'range', min: 0, max: 12, step: 1, unit: 'plain', needs: 'soundtrack' },
			{
				key: 'soundtrack.vad_speech_weight',
				label: 'Favour talking',
				help: 'Higher makes the music dip more readily when someone is speaking.',
				kind: 'range',
				min: 0,
				max: 3,
				step: 0.1,
				unit: 'plain',
				needs: 'soundtrack'
			},
			{
				key: 'soundtrack.beat_burst',
				label: 'Beat burst',
				help: 'A run of very fast cuts at the loudest part of the song.',
				kind: 'choice',
				choices: [
					{ value: 'auto', label: 'When it suits' },
					{ value: 'on', label: 'Always' },
					{ value: 'off', label: 'Never' }
				],
				needs: 'soundtrack'
			},
			{ key: 'soundtrack.hero_on_drop', label: 'Best moment on the drop', help: "Puts the strongest shot where the song's energy jumps.", kind: 'toggle', needs: 'soundtrack' },
			{ key: 'soundtrack.musical_ending', label: "End with the song's ending", help: 'The video ends on a bar line of the song instead of fading mid-phrase.', kind: 'toggle', needs: 'soundtrack' },
			{ key: 'soundtrack.fade_out', label: 'Music fade-out', help: 'How long the music takes to fade at the end.', kind: 'seconds', min: 0, max: 10, step: 0.5, needs: 'soundtrack' }
		]
	},
	{
		id: 'look',
		label: 'Look',
		blurb: 'How one shot turns into the next.',
		controls: [
			{
				key: 'transition.default',
				label: 'Between shots',
				help: 'The transition at every cut. A straight cut is the snappiest.',
				kind: 'choice',
				choices: [
					{ value: 'cut', label: 'Straight cut' },
					{ value: 'fade', label: 'Crossfade' },
					{ value: 'dissolve', label: 'Dissolve' },
					{ value: 'fadeblack', label: 'Dip to black' },
					{ value: 'fadewhite', label: 'Dip to white' },
					{ value: 'wipeleft', label: 'Wipe' },
					{ value: 'slideleft', label: 'Slide' },
					{ value: 'smoothleft', label: 'Smooth slide' },
					{ value: 'circleopen', label: 'Circle open' }
				]
			},
			{ key: 'transition.duration', label: 'Transition length', help: 'How long each transition takes.', kind: 'seconds', min: 0, max: 2, step: 0.05 },
			{ key: 'grading.stabilize', label: 'Steady shaky shots', help: "Smooths handheld wobble. GoPro's HyperSmooth footage doesn't need it.", kind: 'toggle' }
		]
	},
	{
		id: 'titles',
		label: 'Titles',
		blurb: 'The cards at the start and the end.',
		controls: [
			{ key: 'intro.layers.0.content', label: 'Opening title', help: 'The text on the first card.', kind: 'text', needs: 'intro' },
			{ key: 'intro.duration', label: 'Opening card length', help: 'How long the first card stays on screen.', kind: 'seconds', min: 1, max: 10, step: 0.5, needs: 'intro' },
			{ key: 'outro.layers.0.content', label: 'Closing text', help: 'The text on the last card.', kind: 'text', needs: 'outro' },
			{ key: 'outro.duration', label: 'Closing card length', help: 'How long the last card stays on screen.', kind: 'seconds', min: 1, max: 10, step: 0.5, needs: 'outro' }
		]
	},
	{
		id: 'matters',
		label: 'What matters',
		blurb: 'What Vibecut looks for when it picks the moments. Higher counts more.',
		controls: [
			weight('landings', 'Jumps and landings', 'Airtime picked up by the camera’s sensors.'),
			weight('gps_speed', 'Speed', 'Moments when you were moving fastest.'),
			weight('gyro', 'Spins and turns', 'Fast rotations: carves, flips, pans.'),
			weight('accel_peaks', 'Bumps and impacts', 'Sharp jolts in the motion sensors.'),
			weight('motion_peaks', 'Movement in the picture', 'Lots of action on screen.'),
			weight('faces', 'Faces', 'Shots with people looking into the camera.'),
			weight('hilight_tags', 'Your HiLight tags', 'Moments you tagged on the camera.'),
			weight('diversity_per_source', 'Mix of clips', 'Spreads the picks across all your clips.'),
			weight('sharpness_floor', 'Skip blurry shots', 'Higher leaves out more soft or shaky shots.'),
			weight('exposure_floor', 'Skip dark shots', 'Higher leaves out more under-exposed shots.')
		]
	},
	{
		id: 'export',
		label: 'Export',
		blurb: 'The file you download.',
		controls: [
			{
				key: 'outputs.0.crf',
				label: 'Quality',
				help: 'Better quality makes a bigger file.',
				kind: 'choice',
				choices: [
					{ value: 23, label: 'Good' },
					{ value: 20, label: 'Better' },
					{ value: 17, label: 'Best' }
				]
			},
			{
				key: 'outputs.0.resolution',
				label: 'Size',
				help: 'The video’s resolution. 4K takes longest to render.',
				kind: 'choice',
				choices: [
					{ value: '1280x720', label: '720p' },
					{ value: '1920x1080', label: '1080p' },
					{ value: '3840x2160', label: '4K' }
				]
			}
		]
	}
];

/** Every control by key. */
export const CONTROLS: Map<string, Control> = new Map(GROUPS.flatMap((g) => g.controls.map((c) => [c.key, c] as const)));

/** The value at a dotted key (`intro.layers.0.content`) in plain JSON. */
export function getPath(obj: unknown, key: string): unknown {
	let v: unknown = obj;
	for (const seg of key.split('.')) {
		if (v === null || v === undefined || typeof v !== 'object') return undefined;
		v = (v as Record<string, unknown>)[seg];
	}
	return v;
}

/** "7s", "700ms", "1s 500ms", "1m30s", "2.5s" → seconds; NaN when unreadable. */
export function parseDuration(s: unknown): number {
	if (typeof s === 'number') return s;
	if (typeof s !== 'string') return NaN;
	const re = /(\d+(?:\.\d+)?)\s*(ms|s|m|h)/g;
	let total = 0;
	let any = false;
	for (const m of s.matchAll(re)) {
		any = true;
		const n = Number(m[1]);
		total += m[2] === 'ms' ? n / 1000 : m[2] === 's' ? n : m[2] === 'm' ? n * 60 : n * 3600;
	}
	return any ? total : NaN;
}

/** Seconds → the shortest duration string the config reads ("7s", "700ms", "1m30s"). */
export function formatDuration(secs: number): string {
	const ms = Math.round(secs * 1000);
	if (ms === 0) return '0s';
	if (ms % 1000 !== 0) return ms < 1000 ? `${ms}ms` : `${Math.floor(ms / 1000)}s ${ms % 1000}ms`;
	const s = ms / 1000;
	return s >= 60 && s % 60 === 0 ? `${s / 60}m` : s > 60 ? `${Math.floor(s / 60)}m${s % 60}s` : `${s}s`;
}

/** What a value looks like next to its control. */
export function showValue(c: Control, v: unknown): string {
	if (v === undefined || v === null) return '—';
	if (c.kind === 'toggle') return v ? 'On' : 'Off';
	if (c.kind === 'seconds') {
		const secs = parseDuration(v);
		if (!Number.isFinite(secs)) return String(v);
		return secs >= 60 ? `${Math.floor(secs / 60)} min ${Math.round(secs % 60)} s`.replace(' 0 s', '') : `${+secs.toFixed(2)} s`;
	}
	if (c.kind === 'choice') return c.choices?.find((x) => x === v || String(x.value) === String(v))?.label ?? String(v);
	if (typeof v === 'number') {
		if (c.unit === 'percent') return `${Math.round(v * 100)}%`;
		if (c.unit === 'db') return `${v > 0 ? '+' : v < 0 ? '−' : ''}${Math.abs(Math.round(v))} dB`;
		return `${+v.toFixed(2)}`;
	}
	return String(v);
}

/** Two setting values are the same (durations compared as seconds, numbers loosely). */
export function sameValue(c: Control | undefined, a: unknown, b: unknown): boolean {
	if (a === b) return true;
	if (a === undefined || b === undefined || a === null || b === null) return false;
	if (c?.kind === 'seconds') return Math.abs(parseDuration(a) - parseDuration(b)) < 0.0005;
	if (typeof a === 'number' && typeof b === 'number') return Math.abs(a - b) < 1e-4;
	return String(a) === String(b);
}

/** A dotted key the page doesn't have a control for, as words ("selection.max_clip" → "Selection · max clip"). */
export function keyLabel(key: string): string {
	const c = CONTROLS.get(key);
	if (c) return c.label;
	return key
		.split('.')
		.filter((s) => !/^\d+$/.test(s))
		.map((s) => s.replace(/_/g, ' '))
		.map((s, i) => (i === 0 ? s[0].toUpperCase() + s.slice(1) : s))
		.join(' · ');
}
