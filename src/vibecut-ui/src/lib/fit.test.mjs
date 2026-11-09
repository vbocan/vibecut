// Tests for fitBadges in music.ts.
// Run from src/vibecut-ui:  npm test  (or node --test src/lib/fit.test.mjs)

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { fitBadges } from './music.ts';

const vibe = { name: 'Family chronicle', bpm: [85, 120] };
const song = (over = {}) => ({ bpm: 100, duration_secs: 75, grid_tight: true, attribution_required: false, ...over });
const texts = (b) => b.map((x) => x.text);

test('a song in range, long enough, steady: all good', () => {
	assert.deepEqual(texts(fitBadges(song(), vibe, 30)), ['Tempo fits', 'Long enough', 'Steady beat']);
	assert.ok(fitBadges(song(), vibe, 30).every((b) => b.tone === 'ok'));
});

test('too fast or too slow names the vibe; half and double time fit', () => {
	assert.equal(fitBadges(song({ bpm: 150 }), vibe, 0)[0].text, 'Fast for Family chronicle');
	assert.equal(fitBadges(song({ bpm: 70 }), vibe, 0)[0].text, 'Slow for Family chronicle');
	assert.equal(fitBadges(song({ bpm: 55 }), vibe, 0)[0].text, 'Tempo fits'); // 110 in double time
	assert.equal(fitBadges(song({ bpm: 220 }), vibe, 0)[0].text, 'Tempo fits'); // 110 in half time
});

test('a short song says how often it loops; credit is flagged', () => {
	const b = texts(fitBadges(song({ duration_secs: 22, grid_tight: false, attribution_required: true }), vibe, 60));
	assert.deepEqual(b, ['Tempo fits', 'Short: loops 3×', 'Needs credit']);
});

test('no vibe or no length: those badges are left out', () => {
	assert.deepEqual(texts(fitBadges(song({ grid_tight: null }), null, 0)), []);
});
