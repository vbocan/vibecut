// Helpers for picking footage from any drive (BrowseDialog) and the
// recent folders the trip list remembers. Pure, unit-tested
// (browse.test.mjs).

/** A folder picked through "Browse your drives…". */
export interface RecentFolder {
	/** What discover / propose take: relative to /footage inside it, else
	 *  the container path. */
	folder: string;
	/** Display path (`D:\GoPro\Alps`). */
	path: string;
	name: string;
	/** A video in it, for the poster. */
	sample_video: string | null;
}

export const RECENTS_KEY = 'vibecut.recentFolders';
export const MAX_RECENTS = 5;

const FOOTAGE = '/footage';

/** The value discover takes for a container path: inside /footage the
 *  relative folder the trip list uses (so the same trip is the same
 *  card), anywhere else the path itself. */
export function footageValue(containerPath: string): string {
	const p = containerPath.replace(/\/+$/, '');
	if (p === FOOTAGE) return '';
	if (p.startsWith(FOOTAGE + '/')) return p.slice(FOOTAGE.length + 1);
	return containerPath;
}

/** A folder's own name, from a container, display or relative path. */
export function folderName(folder: string): string {
	const parts = folder.split(/[\\/]+/).filter(Boolean);
	return parts.length ? parts[parts.length - 1] : 'All footage';
}

/** `r` first, an older entry for the same folder dropped, at most
 *  {@link MAX_RECENTS}. */
export function addRecent(list: readonly RecentFolder[], r: RecentFolder): RecentFolder[] {
	return [r, ...list.filter((x) => x.folder !== r.folder)].slice(0, MAX_RECENTS);
}

/** Recents from storage; anything malformed is dropped. */
export function parseRecents(raw: string | null): RecentFolder[] {
	if (!raw) return [];
	let v: unknown;
	try {
		v = JSON.parse(raw);
	} catch {
		return [];
	}
	if (!Array.isArray(v)) return [];
	const out: RecentFolder[] = [];
	for (const x of v) {
		if (!x || typeof x !== 'object') continue;
		const { folder, path, name, sample_video } = x as Record<string, unknown>;
		if (typeof folder !== 'string' || typeof path !== 'string' || typeof name !== 'string') continue;
		if (out.some((o) => o.folder === folder)) continue;
		out.push({ folder, path, name, sample_video: typeof sample_video === 'string' ? sample_video : null });
		if (out.length === MAX_RECENTS) break;
	}
	return out;
}

/**
 * Where a key moves the highlight in the folder list of `n` items
 * (no wrap-around, like a file chooser), or null when it isn't a
 * movement key.
 */
export function moveHighlight(key: string, current: number, n: number, page = 8): number | null {
	if (n <= 0) return null;
	const clamp = (i: number) => Math.max(0, Math.min(n - 1, i));
	switch (key) {
		case 'ArrowDown':
			return clamp(current + 1);
		case 'ArrowUp':
			return clamp(current - 1);
		case 'Home':
			return 0;
		case 'End':
			return n - 1;
		case 'PageDown':
			return clamp(current + page);
		case 'PageUp':
			return clamp(current - page);
		default:
			return null;
	}
}

/** "12 videos", "1 video", "" for none. A lower bound gets a "+". */
export function videoCount(n: number, complete = true): string {
	return count(n, complete, 'video');
}

/** "12 songs", "1 song", "" for none. A lower bound gets a "+". */
export function songCount(n: number, complete = true): string {
	return count(n, complete, 'song');
}

function count(n: number, complete: boolean, noun: string): string {
	if (n <= 0) return complete ? '' : '…';
	return `${n}${complete ? '' : '+'} ${noun}${n === 1 ? '' : 's'}`;
}
