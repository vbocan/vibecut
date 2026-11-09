// Tests for the storyboard layout in storyboard.ts.
// Run from src/vibecut-ui:  npm test  (or node --test src/lib/storyboard.test.mjs)

import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
	beatTicks,
	clipsCut,
	matchFrames,
	storyboardCaption,
	storyboardTiles,
	thumbRequests
} from './storyboard.ts';

/** A 10 s plan: a 2 s intro card, a clip, a two-part burst, a clip. */
const plan = {
	duration_secs: 10,
	beats: [0.5, 1, 1.5, 2, 2.5, 3, 3.5, 4, 4.5, 5, 9.5, 10.5],
	song: { tempo_bpm: 120, beat_interval_secs: 0.5 },
	timeline: {
		intro: { output_start_secs: 0, output_end_secs: 2.15, cut_secs: 2 },
		clips: [
			{ segment_index: 0, cut_in_secs: 2, cut_out_secs: 5, ramp: { factor: 3 } },
			{ segment_index: 1, burst_part: 0, cut_in_secs: 5, cut_out_secs: 5.5 },
			{ segment_index: 1, burst_part: 1, cut_in_secs: 5.5, cut_out_secs: 6 },
			{ segment_index: 2, cut_in_secs: 6, cut_out_secs: 10 }
		]
	},
	thumbnails: [
		{ clip_index: 0, source_path: '/f/a.mp4', t_secs: 11.5 },
		{ clip_index: 1, source_path: '/f/b.mp4', t_secs: 3.25 },
		{ clip_index: 2, source_path: '/f/a.mp4', t_secs: 40.25 },
		{ clip_index: 3, source_path: '/f/a.mp4', t_secs: 11.5 }
	]
};

test('tiles partition the video in output order, cards included', () => {
	const tiles = storyboardTiles(plan);
	const r = (x) => Math.round(x * 1e6) / 1e6;
	assert.deepEqual(
		tiles.map((t) => [t.kind, t.clip, r(t.left), r(t.width)]),
		[
			['intro', null, 0, 20],
			['clip', 0, 20, 30],
			['clip', 1, 50, 5],
			['clip', 2, 55, 5],
			['clip', 3, 60, 40]
		]
	);
	assert.equal(tiles[1].ramp, true);
	assert.deepEqual(
		tiles.map((t) => t.burst),
		[false, false, true, true, false]
	);
	// Together they cover the whole strip, no gaps, no overlaps.
	const end = tiles.reduce((at, t) => (assert.ok(Math.abs(t.left - at) < 1e-9, `${t.left} vs ${at}`), t.left + t.width), 0);
	assert.ok(Math.abs(end - 100) < 1e-9);
});

test('beat ticks stay inside the video, every fourth beat a bar line', () => {
	const ticks = beatTicks(plan);
	assert.equal(ticks.length, 11, 'the beat past the end is dropped');
	assert.deepEqual(
		ticks.slice(0, 5).map((t) => t.bar),
		[true, false, false, false, true]
	);
	assert.equal(ticks[0].left, 5);
	// A long video is thinned, never crowded.
	const long = { ...plan, duration_secs: 1000, beats: Array.from({ length: 2000 }, (_, i) => i * 0.5) };
	assert.ok(beatTicks(long, 100).length <= 100);
});

test('one frame request per source file, split when it asks for too many', () => {
	const reqs = thumbRequests(plan);
	assert.deepEqual(reqs, [
		{ source: '/f/a.mp4', clips: [0, 2, 3], times: [11.5, 40.25, 11.5] },
		{ source: '/f/b.mp4', clips: [1], times: [3.25] }
	]);
	assert.deepEqual(
		thumbRequests(plan, 2).map((r) => [r.source, r.clips]),
		[
			['/f/a.mp4', [0, 2]],
			['/f/b.mp4', [1]],
			['/f/a.mp4', [3]]
		]
	);
});

test('frames go to the clips that asked for them, missing ones stay empty', () => {
	const req = thumbRequests(plan)[0];
	// The server rounds the times it was sent; one frame failed.
	const got = matchFrames(req, [{ t_secs: 11.5, data_uri: 'A' }]);
	assert.deepEqual(got, { 0: 'A', 3: 'A' });
	const both = matchFrames(req, [
		{ t_secs: 40.25, data_uri: 'B' },
		{ t_secs: 11.5, data_uri: 'A' }
	]);
	assert.deepEqual(both, { 0: 'A', 2: 'B', 3: 'A' });
	assert.deepEqual(matchFrames(req, [{ t_secs: 99, data_uri: 'Z' }]), {});
});

test('clips light up as the render cuts them', () => {
	assert.equal(clipsCut(null, 4), 0);
	assert.equal(clipsCut({ stage: 'analysing', detail: '2 of 4 clips' }, 4), 0);
	assert.equal(clipsCut({ stage: 'clips', detail: '3 of 4 clips' }, 4), 3);
	assert.equal(clipsCut({ stage: 'clips', detail: null }, 4), 0);
	assert.equal(clipsCut({ stage: 'joining', detail: null }, 4), 4);
});

test('caption counts shots, tempo and length', () => {
	assert.equal(storyboardCaption(plan), '4 shots · 120 BPM · 0:10');
	assert.equal(storyboardCaption({ ...plan, song: null, duration_secs: 75.4 }), '4 shots · 1:15');
});
