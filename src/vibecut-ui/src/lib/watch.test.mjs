// Tests for the footage-watcher helpers in watch.ts.
// Run from src/vibecut-ui:  npm test  (or node --test src/lib/watch.test.mjs)

import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
	applyWatchEvent,
	beatScore,
	deepLinkId,
	reelNotification,
	reelTitle,
	watchStatusText,
	watchTone
} from './watch.ts';

const entry = (over = {}) => ({
	id: 'w1',
	path: '/footage/Coastline',
	folder: 'Coastline',
	kind: 'folder',
	name: 'Coastline',
	content_id: '',
	status: 'queued',
	note: null,
	n_videos: 3,
	total_bytes: 1024,
	found_at: '2026-11-09T10:00:00+00:00',
	updated_at: '2026-11-09T10:00:00+00:00',
	attempts: 0,
	job_id: null,
	progress: null,
	stage: null,
	proposal: null,
	render: null,
	poster_path: null,
	dismissed: false,
	...over
});

const done = entry({
	status: 'done',
	proposal: { title: 'Coastline · 2026-11-09', config_path: '/projects/p.toml' },
	render: {
		ok: true,
		config_path: '/projects/p.toml',
		output_path: '/projects/out/coastline.mp4',
		manifest_summary: {
			n_segments: 12,
			actual_duration_secs: 30.04,
			target_duration_secs: 30,
			n_sources: 3,
			beat_sync: { tempo_bpm: 132, n_cuts: 12, within_tolerance: 12, mean_abs_error_ms: 8 }
		}
	}
});

test('each status reads as a short line with a tone', () => {
	assert.equal(watchStatusText(entry({ status: 'waiting' })), 'Waiting for the copy to finish…');
	assert.equal(watchStatusText(entry({ status: 'queued' })), 'Up next');
	assert.equal(watchStatusText(entry({ status: 'rendering', stage: 'Cutting clips', progress: 0.416 })), 'Cutting clips · 42%');
	assert.equal(watchStatusText(entry({ status: 'rendering' })), 'Cutting');
	assert.equal(watchStatusText(done), 'Reel ready · 30 s · 12/12 on the beat');
	assert.equal(watchStatusText(entry({ status: 'failed', note: 'No clips' })), "Couldn't make it — No clips");
	assert.equal(watchStatusText(entry({ status: 'skipped', note: 'Already in your projects (a.toml)' })), 'Already in your projects (a.toml)');
	assert.deepEqual(
		['waiting', 'rendering', 'done', 'failed', 'skipped', 'stopped'].map((status) => watchTone(entry({ status }))),
		['busy', 'busy', 'ok', 'bad', 'quiet', 'quiet']
	);
});

test('the reel is named by its title card and scored by its cuts', () => {
	assert.equal(reelTitle(done), 'Coastline · 2026-11-09');
	assert.equal(reelTitle(entry()), 'Coastline');
	assert.equal(beatScore(done.render), '12/12 on the beat');
	assert.equal(beatScore(null), null);
	assert.equal(beatScore({ ...done.render, manifest_summary: { ...done.render.manifest_summary, beat_sync: { n_cuts: 0 } } }), null);
	assert.deepEqual(reelNotification(done), {
		title: 'Your reel is ready',
		body: 'Coastline · 2026-11-09 — 30 s, 12/12 on the beat'
	});
	assert.equal(reelNotification(entry()).body, 'Coastline');
});

test('feed events update the list in place, newest first', () => {
	const older = entry({ id: 'a', name: 'A', found_at: '2026-11-09T09:00:00+00:00' });
	let list = [older];
	const b = entry({ id: 'b', name: 'B', status: 'waiting', found_at: '2026-11-09T10:00:00+00:00' });
	list = applyWatchEvent(list, { type: 'entry', entry: b });
	assert.deepEqual(list.map((e) => e.id), ['b', 'a']);
	// A change keeps the row where it is.
	list = applyWatchEvent(list, { type: 'entry', entry: { ...older, status: 'rendering', progress: 0.5 } });
	assert.deepEqual(list.map((e) => [e.id, e.status]), [['b', 'waiting'], ['a', 'rendering']]);
	list = applyWatchEvent(list, { type: 'reel_ready', entry: { ...older, status: 'done' } });
	assert.equal(list[1].status, 'done');
	// Dismissed or baseline entries leave; removals too; status changes nothing.
	list = applyWatchEvent(list, { type: 'entry', entry: { ...b, dismissed: true } });
	assert.deepEqual(list.map((e) => e.id), ['a']);
	assert.deepEqual(applyWatchEvent(list, { type: 'entry', entry: entry({ id: 'c', status: 'baseline' }) }), list);
	assert.deepEqual(applyWatchEvent(list, { type: 'removed', id: 'a' }), []);
	assert.equal(applyWatchEvent(list, { type: 'status', enabled: false }), list);
});

test('deep links name one reel and nothing else', () => {
	assert.equal(deepLinkId('?watch=w0123abcd'), 'w0123abcd');
	assert.equal(deepLinkId('?x=1&watch=w9'), 'w9');
	assert.equal(deepLinkId(''), null);
	assert.equal(deepLinkId('?watch=../../etc'), null);
	assert.equal(deepLinkId('?watch=' + 'a'.repeat(65)), null);
});
