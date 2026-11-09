// Tests for the beat timeline's song-form helpers in songform.ts.
// Run from src/vibecut-ui:  node --test src/lib/songform.test.mjs

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { describeSongForm, gridLines, heroSpotText, sectionFill } from './songform.ts';

const beats = Array.from({ length: 17 }, (_, i) => 0.5 + i * 0.5);

test('bar lines and phrase starts come from the song, heavier kind wins', () => {
	// Downbeats every 4 beats from 1.0 s (beat 1), a phrase at 1.0 s and 9.0 s.
	const music = { downbeats: [1.0, 3.0, 5.0, 7.0, 9.0], phrases: [9.0, 1.0] };
	const lines = gridLines(beats, music);
	const kinds = (k) => lines.filter((l) => l.kind === k).map((l) => l.t);
	assert.deepEqual(kinds('phrase'), [1.0, 9.0]);
	assert.deepEqual(kinds('bar'), [3.0, 5.0, 7.0]);
	// Beats 1, 3, 5 and 7 s are drawn as their bar or phrase line (9 s is
	// past the last beat).
	assert.equal(kinds('beat').length, beats.length - 4);
	assert.ok(lines.every((l, i) => i === 0 || lines[i - 1].t <= l.t), 'sorted');
	// A beat a few ms off its bar line (frame rounding) is still that line.
	const nudged = gridLines([2.99, 3.5], { downbeats: [3.0], phrases: [] });
	assert.deepEqual(nudged, [
		{ t: 3.0, kind: 'bar' },
		{ t: 3.5, kind: 'beat' }
	]);
});

test('without the song form every fourth beat is a bar', () => {
	const lines = gridLines(beats.slice(0, 6), undefined);
	assert.deepEqual(
		lines.map((l) => l.kind),
		['bar', 'beat', 'beat', 'beat', 'bar', 'beat']
	);
});

test('sections are tinted by level, the drop brightest', () => {
	const drop = sectionFill({ level: 'high', drop: true });
	const high = sectionFill({ level: 'high', drop: false });
	const quiet = sectionFill({ level: 'quiet', drop: false });
	const mid = sectionFill({ level: 'mid', drop: false });
	assert.equal(new Set([drop, high, quiet, mid]).size, 4);
	const alpha = (c) => Number(c.match(/([\d.]+)\)$/)[1]);
	assert.ok(alpha(drop) > alpha(high) && alpha(high) > alpha(mid));
});

test('the song form reads as one sentence', () => {
	const music = {
		bpm: 96,
		beats_per_bar: 4,
		downbeat_confidence: 0.5,
		downbeats: [],
		phrases: [],
		sections: [
			{ start_secs: 0.6, end_secs: 5.6, level: 'quiet', intensity: 0.2, drop: false, song_start_secs: 0.6 },
			{ start_secs: 10.6, end_secs: 15.6, level: 'high', intensity: 0.8, drop: true, song_start_secs: 10.6 }
		],
		song_sound_end_secs: 80,
		hero: { segment: 3, start_secs: 10.62, on: 'drop' },
		ending: { kind: 'phrase_end', last_cut_secs: 30.63, video_end_secs: 30.63, fade_secs: 1.25 }
	};
	assert.equal(
		describeSongForm(music),
		'Song: 2 sections, 1 of them loud; the drop at 0:11; the best moment starts on the drop at 0:11; it ends on a phrase start at 0:31.'
	);
	assert.equal(describeSongForm(null), '');
	assert.equal(describeSongForm({ ...music, sections: [] }), '');
	assert.equal(heroSpotText('phrase'), 'on a phrase');
});
