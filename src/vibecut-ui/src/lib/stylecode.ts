// Style codes ("VC1-…") on the client: find the code in whatever was
// pasted, and how to show the song it got. The server reads and checks
// the code itself. Kept free of runes and $lib imports so `node --test`
// can load it.

/** A whole pasted value that is one code (a chat app may have wrapped it
 *  over lines or quoted it). */
const WHOLE = /^vc\d+-[a-z0-9_-]{6,}$/i;
/** A code inside other text ("try my style: VC1-Q1VCGg!"). */
const INSIDE = /(?:^|[^a-z0-9_-])(vc\d+-[a-z0-9_-]{6,})/i;
const QUOTES = /^["'`“”‘’<]+|["'`“”‘’>]+$/g;

/** The style code in `text`, with an upper-case `VC` prefix; null if
 *  there's none. */
export function extractStyleCode(text: string): string | null {
	const squeezed = text.replace(/\s+/g, '').replace(QUOTES, '');
	const code = WHOLE.test(squeezed) ? squeezed : (INSIDE.exec(text)?.[1] ?? null);
	return code ? `VC${code.slice(2)}` : null;
}

/** Text that is plainly meant as a code but isn't one yet: worth a hint
 *  rather than silence. */
export function looksLikeAnAttempt(text: string): boolean {
	const t = text.trim();
	return t.length > 0 && extractStyleCode(t) === null;
}

export type MatchTone = 'good' | 'warn' | 'plain';

/** How the song a style gets here should read: its own song is good news,
 *  a stand-in or no music is worth noticing. */
export function musicTone(how: string): MatchTone {
	if (how === 'same_song' || how === 'same_title') return 'good';
	if (how === 'original_sound') return 'plain';
	return 'warn';
}
