// Tests for the platform-version framing rules in reframe.ts.
// Run from src/vibecut-ui:  npm test  (or node --test src/lib/reframe.test.mjs)

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { aspectOf, centreAt, cropBoxAt, fitFor, previewTime, progressLabel, reframes } from './reframe.ts';

/** Left of centre for 4 s, a pan right over 2 s, then a cut to the left. */
const PATH = {
	axis: 'x',
	window: 0.3164,
	shots: 2,
	followed_shots: 2,
	pieces: [
		[0, 4, 0.3, 0.3],
		[4, 6, 0.3, 0.8],
		[6, 8, 0.8, 0.8],
		[8, 12, 0.2, 0.2]
	]
};

test('only a change of shape is framed, and the choice is the framing', () => {
	assert.equal(aspectOf('1920x1080'), 1920 / 1080);
	assert.equal(aspectOf('1080×1920'), 1080 / 1920);
	assert.equal(aspectOf('wide'), null);
	assert.equal(reframes('1920x1080', 16, 9), false);
	assert.equal(reframes('1920x1080', 9, 16), true);
	assert.equal(reframes('1920x1080', 4, 5), true);
	assert.equal(reframes('1920x1080', 1, 1), true);
	// Unknown renders are the 16:9 master.
	assert.equal(reframes(undefined, 16, 9), false);
	// A vertical render made into a YouTube version is framed too.
	assert.equal(reframes('1080x1920', 16, 9), true);
	assert.equal(fitFor('smart', true), 'smart');
	assert.equal(fitFor('blur', true), 'blur');
	assert.equal(fitFor('smart', false), undefined);
});

test('the window follows the path: linear pans, jumps at cuts, held past the end', () => {
	assert.equal(centreAt(PATH, 2), 0.3);
	assert.ok(Math.abs(centreAt(PATH, 5) - 0.55) < 1e-9);
	assert.equal(centreAt(PATH, 7.99), 0.8);
	assert.equal(centreAt(PATH, 8), 0.2);
	assert.equal(centreAt(PATH, 99), 0.2);
	assert.equal(centreAt({ ...PATH, pieces: [] }, 3), 0.5);
});

test('the overlay box stays inside the frame', () => {
	const b = cropBoxAt(PATH, 2);
	assert.ok(Math.abs(b.left - (0.3 - 0.3164 / 2) * 100) < 1e-9);
	assert.deepEqual([b.top, b.height], [0, 100]);
	assert.ok(Math.abs(b.width - 31.64) < 1e-9);
	// A centre past the edge is clamped so the box never spills out.
	const edge = cropBoxAt({ ...PATH, pieces: [[0, 1, 0.01, 0.01]] }, 0.5);
	assert.equal(edge.left, 0);
	// Sliding up and down for a wider shape.
	const tall = cropBoxAt({ ...PATH, axis: 'y' }, 2);
	assert.deepEqual([tall.left, tall.width], [0, 100]);
	assert.ok(tall.top > 0 && tall.height < 100);
});

test('the preview shows the crop where it moved furthest from the centre', () => {
	// 0.8 (0.3 off centre) at 6–8 s beats 0.2 later on (also 0.3, but later).
	assert.equal(previewTime(PATH, 12), 6);
	// Nothing off centre, no path, or too short: the middle.
	const centred = { ...PATH, pieces: [[0, 12, 0.5, 0.5]] };
	assert.equal(previewTime(centred, 12), 6);
	assert.equal(previewTime(null, 30), 15);
	assert.equal(previewTime(PATH, 1.5), 0.75);
});

test('a smart version first says it is finding the action', () => {
	assert.equal(progressLabel('smart', 0.1, 'Reels'), 'Finding the action… 10%');
	assert.equal(progressLabel('smart', 0.6, 'Reels'), 'Making the Reels version… 60%');
	assert.equal(progressLabel('blur', 0.1, 'Square'), 'Making the Square version… 10%');
	assert.equal(progressLabel(undefined, 2, 'YouTube'), 'Making the YouTube version… 100%');
});
