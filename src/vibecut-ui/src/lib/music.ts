// Music licence wording for the UI. The server classifies each track's
// licence and writes the advice (`Track.licence.note`); these helpers only
// decide how it looks and what the caption says.

import type { Track } from './api';

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
			return { text: t.licence.label || 'Custom licence', tone: 'caution' };
		default:
			return { text: 'Licence unknown', tone: 'unknown' };
	}
}

/** "Starter pack" / "Your music" / "Library". */
export function collectionLabel(c: Track['collection']): string {
	return c === 'starter' ? 'Starter pack' : c === 'yours' ? 'Your music' : 'Library';
}

/** The caption's music line: the licence's credit when it needs one,
 *  otherwise just the song (and artist when known). */
export function musicCaptionLine(t: Track): string {
	if (t.attribution_required && t.credit) return t.credit;
	return t.artist ? `Music: ${t.title} by ${t.artist}` : `Music: ${t.title}`;
}
