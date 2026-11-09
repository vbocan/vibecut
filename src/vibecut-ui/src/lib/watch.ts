// Words and list bookkeeping for the footage watcher ("New footage
// found", the reel-ready toast, browser notifications).
//
// Pure functions, type-only imports: `node --test src/lib/watch.test.mjs`
// runs their tests without a browser.

import type { RenderResult, WatchEntry, WatchEvent } from './api';

/** "12/12 on the beat" from a render's manifest summary. */
export function beatScore(r: RenderResult | null | undefined): string | null {
	const b = r?.manifest_summary?.beat_sync;
	if (!b || !b.n_cuts) return null;
	return `${b.within_tolerance ?? 0}/${b.n_cuts} on the beat`;
}

/** The reel's length, rounded to whole seconds. */
export function reelSeconds(r: RenderResult | null | undefined): number | null {
	const d = r?.manifest_summary?.actual_duration_secs;
	return typeof d === 'number' && d > 0 ? Math.round(d) : null;
}

/** The title on the reel's opening card, else the folder's name. */
export function reelTitle(e: WatchEntry): string {
	return e.proposal?.title || e.name;
}

export type WatchTone = 'busy' | 'ok' | 'bad' | 'quiet';

/** The status line under a watched folder. */
export function watchStatusText(e: WatchEntry): string {
	switch (e.status) {
		case 'waiting':
			return 'Waiting for the copy to finish…';
		case 'queued':
			return 'Up next';
		case 'rendering': {
			const pct = e.progress != null && e.progress > 0 ? ` · ${Math.round(e.progress * 100)}%` : '';
			return `${e.stage || 'Cutting'}${pct}`;
		}
		case 'done': {
			const facts = [reelSeconds(e.render) != null ? `${reelSeconds(e.render)} s` : null, beatScore(e.render)];
			const tail = facts.filter(Boolean).join(' · ');
			return tail ? `Reel ready · ${tail}` : 'Reel ready';
		}
		case 'failed':
			return e.note ? `Couldn't make it — ${e.note}` : "Couldn't make it";
		case 'stopped':
			return 'Stopped';
		case 'skipped':
			return e.note ?? 'Skipped';
		default:
			return '';
	}
}

export function watchTone(e: WatchEntry): WatchTone {
	switch (e.status) {
		case 'waiting':
		case 'queued':
		case 'rendering':
			return 'busy';
		case 'done':
			return 'ok';
		case 'failed':
			return 'bad';
		default:
			return 'quiet';
	}
}

/** Shown in the list: not the folders that were there before watching
 *  began, nor dismissed ones. */
export function listed(e: WatchEntry): boolean {
	return e.status !== 'baseline' && !e.dismissed;
}

/** The list after one event from the feed: entries are replaced in place
 *  (or added, newest first), removed ones dropped. */
export function applyWatchEvent(list: WatchEntry[], ev: WatchEvent): WatchEntry[] {
	if (ev.type === 'removed') return list.filter((e) => e.id !== ev.id);
	if (ev.type !== 'entry' && ev.type !== 'reel_ready') return list;
	const entry = ev.entry;
	const rest = list.filter((e) => e.id !== entry.id);
	if (!listed(entry)) return rest;
	if (rest.length < list.length) return list.map((e) => (e.id === entry.id ? entry : e));
	return [entry, ...rest].sort((a, b) => b.found_at.localeCompare(a.found_at));
}

/** The browser notification for a finished reel. */
export function reelNotification(e: WatchEntry): { title: string; body: string } {
	const facts = [reelSeconds(e.render) != null ? `${reelSeconds(e.render)} s` : null, beatScore(e.render)].filter(
		Boolean
	);
	return {
		title: 'Your reel is ready',
		body: facts.length ? `${reelTitle(e)} — ${facts.join(', ')}` : reelTitle(e)
	};
}

/** The `?watch=<id>` deep link a webhook's `app_url` carries. */
export function deepLinkId(search: string): string | null {
	const id = new URLSearchParams(search).get('watch');
	return id && /^[A-Za-z0-9_-]{1,64}$/.test(id) ? id : null;
}
