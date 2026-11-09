// Music licence wording for the UI. The server classifies each track's
// licence and writes the advice (`Track.licence.note`); these helpers only
// decide how it looks and what the caption says.
//
// Pure functions, no imports with side effects: `node --test
// src/lib/music.test.mjs` runs their tests without a browser.

import type { Track, TrackLicence } from './api';
import type { SoundtrackCredit } from './types/generated/SoundtrackCredit';

export type LicenceTone = 'ok' | 'credit' | 'caution' | 'unknown';

/** Short badge text and colour for a track's licence. */
export function licenceBadge(t: Track): { text: string; tone: LicenceTone } {
	switch (t.licence.kind) {
		case 'cc0':
			return { text: 'CC0', tone: 'ok' };
		case 'cc_by':
			return { text: t.licence.label, tone: 'credit' };
		case 'cc_by_sa':
		case 'cc_by_nc':
		case 'cc_by_nd':
			return { text: t.licence.label, tone: 'caution' };
		case 'pixabay':
			return { text: 'Pixabay', tone: 'caution' };
		case 'youtube_audio_library':
			return { text: 'YouTube Audio Library', tone: t.attribution_required ? 'credit' : 'ok' };
		case 'custom':
			return { text: t.licence.label || 'Custom licence', tone: t.attribution_required ? 'credit' : 'caution' };
		default:
			return { text: 'Licence unknown', tone: 'unknown' };
	}
}

/** "Starter pack" / "Your music" / "Library". */
export function collectionLabel(c: Track['collection']): string {
	return c === 'starter' ? 'Starter pack' : c === 'yours' ? 'Your music' : 'Library';
}

/** What a caption line needs: a library track, or a render's own record
 *  of its soundtrack (the manifest's `soundtrack` entries). */
export interface Creditable {
	title: string;
	artist: string | null;
	credit: string | null;
	attribution_required: boolean;
}

/** The caption's music line: the licence's credit when it needs one,
 *  otherwise just the song (and artist when known). */
export function musicCaptionLine(t: Creditable): string {
	if (t.attribution_required && t.credit) return t.credit;
	return t.artist ? `Music: ${t.title} by ${t.artist}` : `Music: ${t.title}`;
}

/** The caption's music lines for the video being shared: every file its
 *  manifest recorded (title, credit, whether the licence needs it), else
 *  — a manifest from an older build, or none loaded yet — the project's song. */
export function captionMusicLines(
	manifestCredits: readonly Creditable[] | null | undefined,
	song: Creditable | null
): string[] {
	const credits = manifestCredits && manifestCredits.length > 0 ? manifestCredits : song ? [song] : [];
	return credits.map(musicCaptionLine);
}

/** Whether the song labels describe the current project rather than what
 *  the next "Make my video" would use: once the project has a video, or
 *  while its own render runs, finishes or fails. On the create screen — no
 *  video yet, or back from a failed attempt — they describe the next video,
 *  because "Make my video" always proposes afresh. */
export function labelsFollowProject(s: {
	hasProposal: boolean;
	versions: number;
	makeStep: string;
	makeError: string | null;
}): boolean {
	if (!s.hasProposal) return false;
	if (s.versions > 0) return true;
	// A failure before the render (scan, proposal) leaves an older proposal
	// in place that isn't what was attempted.
	return (s.makeStep === 'rendering' || s.makeStep === 'done' || s.makeStep === 'failed') && !s.makeError;
}

/** The song file a project uses: its config's first `[soundtrack]` file
 *  once that config is loaded (null: no music), else the proposal's own
 *  record of it. `undefined` when neither says — a proposal saved by a
 *  build that didn't record its song. */
export function projectSongPath(p: {
	configLoaded: boolean;
	configFiles: readonly string[] | null | undefined;
	proposalSong: { path: string } | null | undefined;
}): string | null | undefined {
	if (p.configLoaded) return p.configFiles?.[0] ?? null;
	if (p.proposalSong !== undefined) return p.proposalSong?.path ?? null;
	return undefined;
}

/** "summer_drive-124bpm.mp3" → "Summer drive 124bpm", as the server titles
 *  files without metadata. */
export function titleFromPath(path: string): string {
	const name = path.split(/[\\/]/).pop() ?? '';
	const dot = name.lastIndexOf('.');
	const stem = dot > 0 ? name.slice(0, dot) : name;
	const words = stem.replace(/[_-]/g, ' ').split(/\s+/).filter(Boolean).join(' ');
	return words ? words[0].toUpperCase() + words.slice(1) : 'Untitled';
}

/** Licence kind from a normalised licence id (`CC-BY-4.0`, `Pixabay`, …). */
export function licenceKindFromId(id: string): TrackLicence['kind'] {
	if (id === 'unknown' || id === '') return 'unknown';
	if (id.startsWith('CC0')) return 'cc0';
	if (id.startsWith('CC-BY-NC')) return 'cc_by_nc';
	if (id.startsWith('CC-BY-ND')) return 'cc_by_nd';
	if (id.startsWith('CC-BY-SA')) return 'cc_by_sa';
	if (id.startsWith('CC-BY')) return 'cc_by';
	if (id === 'Pixabay') return 'pixabay';
	if (id === 'YouTube-Audio-Library') return 'youtube_audio_library';
	return 'custom';
}

/** The library entry for `path`, else a stand-in built from what is known
 *  about it — the render's credit record, the proposal's title, the file
 *  name — so a song the video has is never labelled "Original sound" just
 *  because the library list is stale or the file sits outside it. */
export function trackForPath(
	path: string,
	tracks: readonly Track[],
	hints: { credit?: SoundtrackCredit | null; title?: string | null } = {}
): Track {
	const known = tracks.find((t) => t.path === path);
	if (known) return known;
	const c = hints.credit ?? null;
	const licenceId = c?.licence ?? 'unknown';
	return {
		path,
		title: c?.title ?? hints.title ?? titleFromPath(path),
		collection: 'yours',
		artist: c?.artist ?? null,
		licence: { id: licenceId, label: c?.licence_label ?? 'Unknown licence', kind: licenceKindFromId(licenceId), note: '' },
		attribution_required: c?.attribution_required ?? false,
		credit: c?.credit ?? null,
		source_url: c?.source_url ?? null,
		declared_bpm: null,
		metadata: null,
		loops_cleanly: false,
		duration_secs: 0,
		bpm: 0,
		beat_interval_secs: 0,
		first_beat_secs: null,
		grid_tight: null,
		pulse_strength: 0,
		energy: [],
		energy_mean: 0,
		mood: 'warm',
		loud_region: [0, 0]
	};
}

/** A site to download songs from, and what its licence asks of the user. */
export interface MusicSource {
	name: string;
	url: string;
	/** What the free licence lets you do, in a few words. */
	terms: string;
	/** `ok`: no credit; `credit`: credit the artist; `caution`: read the terms. */
	tone: LicenceTone;
	badge: string;
}

/** Where to get songs Vibecut may use in your videos. Vibecut can't ship
 *  these (the sites forbid passing the files on), so the user downloads
 *  them and puts them in a music folder. Terms as of November 2026. */
export const MUSIC_SOURCES: readonly MusicSource[] = [
	{
		name: 'Pixabay Music',
		url: 'https://pixabay.com/music/',
		terms: 'Free, no credit needed. A few tracks are registered with YouTube Content ID (shield icon): keep their certificate in case of a claim.',
		tone: 'ok',
		badge: 'No credit'
	},
	{
		name: 'YouTube Audio Library',
		url: 'https://www.youtube.com/audiolibrary',
		terms: 'Free in YouTube Studio; some tracks ask for a credit (it says which). Made for YouTube videos.',
		tone: 'credit',
		badge: 'Some need credit'
	},
	{
		name: 'Free Music Archive',
		url: 'https://freemusicarchive.org/',
		terms: 'Pick tracks marked CC0 (no conditions) or CC BY (credit the artist). Skip NC tracks if your video earns money.',
		tone: 'credit',
		badge: 'CC0 / CC BY'
	},
	{
		name: 'Incompetech (Kevin MacLeod)',
		url: 'https://incompetech.com/music/royalty-free/music.html',
		terms: 'CC BY 4.0: free, monetised videos allowed, credit required.',
		tone: 'credit',
		badge: 'Credit'
	},
	{
		name: 'Mixkit',
		url: 'https://mixkit.co/free-stock-music/',
		terms: 'Free under the Mixkit licence, no credit needed.',
		tone: 'ok',
		badge: 'No credit'
	},
	{
		name: 'Uppbeat',
		url: 'https://uppbeat.io/',
		terms: 'Free account: a few downloads a month, credit required. Read its licence before posting.',
		tone: 'caution',
		badge: 'Account, credit'
	}
];
