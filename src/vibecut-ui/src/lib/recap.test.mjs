// Tests for the recap card's helpers in recap.ts.
// Run from src/vibecut-ui:  npm test  (or node --test src/lib/recap.test.mjs)

import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
	clock,
	offeredYear,
	recapProposal,
	recapRemakeArgs,
	recapSummary,
	recapTiles,
	scaledRecapLength,
	shortDay,
	thumbKey
} from './recap.ts';

function moment(path, bucket, start, end, caption = null) {
	return {
		path,
		start_secs: start,
		end_secs: end,
		file_secs: 60,
		recorded_at: '2026-03-14T09:00:00Z',
		dated_by: 'footage',
		score: 1,
		versions: 1,
		trip: 'Alps',
		clip_start_secs: start,
		clip_end_secs: end,
		bucket,
		caption
	};
}

const monthPlan = {
	period: { from: '2026-01-01', to: '2026-12-31', year: 2026 },
	title: '2026 in moments',
	subtitle: '3 moments · 2 months',
	grouping: 'month',
	buckets: [
		{ key: '2026-03', label: 'March', first_recorded: '2026-03-14T09:00:00Z', available: 4, chosen: 2, trips: ['Alps', 'Lake', 'Coast'] },
		{ key: '2026-05', label: 'May', first_recorded: '2026-05-02T09:00:00Z', available: 1, chosen: 0, trips: ['Garden'] },
		{ key: '2026-06', label: 'June', first_recorded: '2026-06-02T09:00:00Z', available: 1, chosen: 1, trips: ['Coast'] }
	],
	moments: [
		moment('/f/alps.MP4', 'March', 10, 14, 'March'),
		moment('/f/alps.MP4', 'March', 30, 34),
		moment('/f/coast.MP4', 'June', 2, 7, 'June')
	],
	length_secs: 45,
	moment_secs: 4,
	estimated_secs: 58.4,
	eligible: true,
	min_buckets: 3,
	versions: 6,
	notes: []
};

test('the card offers the latest year with enough months or trips', () => {
	const years = (...eligible) => ({ plan: null, versions: 3, years: eligible.map(([year, ok]) => ({ year, months: 1, trips: 1, moments: 1, eligible: ok })) });
	assert.equal(offeredYear(years([2026, false], [2025, true], [2024, true])), 2025);
	assert.equal(offeredYear(years([2026, false])), null);
	assert.equal(offeredYear(years()), null);
	assert.equal(offeredYear(null), null);
});

test('tiles follow the plan: trips per month, left-out months, the first moment as thumbnail', () => {
	const tiles = recapTiles(monthPlan);
	assert.deepEqual(
		tiles.map((t) => [t.label, t.detail, t.chosen, t.leftOut]),
		[
			['March', 'Alps, Lake +1', 2, false],
			['May', 'No room at this length', 0, true],
			['June', 'Coast', 1, false]
		]
	);
	assert.deepEqual(tiles[0].thumb, { path: '/f/alps.MP4', t: 12 });
	assert.equal(tiles[1].thumb, null);
	assert.equal(thumbKey(tiles[0]), '/f/alps.MP4@12.00');
	assert.equal(thumbKey(tiles[1]), '2026-05');
});

test('a trip recap dates each trip', () => {
	const plan = {
		...monthPlan,
		grouping: 'trip',
		buckets: [{ key: 'alps', label: 'Alps', first_recorded: '2026-11-08T21:59:42Z', available: 2, chosen: 2, trips: ['Alps'] }],
		moments: [moment('/f/a.MP4', 'Alps', 0, 4, 'Alps'), moment('/f/a.MP4', 'Alps', 8, 12)]
	};
	assert.equal(recapTiles(plan)[0].detail, 'Nov 8');
	assert.equal(recapSummary(plan), '2 moments · 1 trip · about 0:58');
});

test('the summary counts moments and the months that made it', () => {
	assert.equal(recapSummary(monthPlan), '3 moments · 2 months · about 0:58');
	assert.equal(clock(61.4), '1:01');
	assert.equal(clock(-3), '0:00');
	assert.equal(shortDay('2026-03-04T00:00:00Z'), 'Mar 4');
	assert.equal(shortDay('soon'), '');
});

const madeRecap = {
	config_path: '/projects/recaps/recap_2026.toml',
	config_toml: 'x',
	output_path: '/projects/recaps/out/recap_2026.mp4',
	plan: { ...monthPlan, shuffle: null },
	soundtrack: { path: '/projects/music/a.mp3', title: 'A', picked_by: 'vibecut', reason: 'fits' },
	auto_pick: null,
	music_note: null,
	vibe: 'family_chronicle',
	vibe_name: 'Family chronicle',
	rationale: 'The best moments of each month of 2026.'
};

test('a made recap becomes the Studio project, titled by the recap', () => {
	const p = recapProposal({
		config_path: '/projects/recaps/recap_2026.toml',
		config_toml: 'x',
		output_path: '/projects/recaps/out/recap_2026.mp4',
		plan: monthPlan,
		soundtrack: { path: '/projects/music/a.mp3', title: 'A', picked_by: 'vibecut', reason: 'fits' },
		auto_pick: null,
		music_note: null,
		vibe: 'family_chronicle',
		vibe_name: 'Family chronicle',
		rationale: 'The best moments of each month of 2026.'
	});
	assert.equal(p.title, '2026 in moments');
	assert.equal(p.vibe_name, 'Year recap');
	assert.equal(p.vibe, 'family_chronicle', 'another mix moves on from the vibe that chose the song');
	assert.equal(p.config_path, '/projects/recaps/recap_2026.toml');
	assert.equal(p.soundtrack.path, '/projects/music/a.mp3');
	assert.deepEqual(p.recap, { year: 2026, from: '2026-01-01', to: '2026-12-31', length: 45, shuffle: null });
	const shuffled = recapProposal({ ...madeRecap, plan: { ...monthPlan, shuffle: 77 } });
	assert.equal(shuffled.recap.shuffle, 77);
});

test('making a recap again keeps its song, drops and transitions; another mix lets the vibe pick', () => {
	const p = recapProposal(madeRecap);
	const exclude = [{ file: 'GX010201.MP4', start: '9s 500ms', end: '14s' }];
	const transition = { default: 'fade', duration: '200ms', palette: [], speed_ramp_probability: 0.5 };
	const kept = { exclude, transition };
	const none = { exclude: [] };
	const song = '/projects/music/b.mp3';
	// Longer: the song playing (maybe changed since), the drops, the
	// transitions, no shuffle.
	assert.deepEqual(recapRemakeArgs(p, { length: 56 }, song, kept, 42), {
		year: 2026,
		length: 56,
		vibe: 'family_chronicle',
		soundtrack: song,
		exclude,
		transition,
		seed: 42,
		render: false
	});
	// Config not loaded yet: nothing to keep but the song.
	assert.equal('transition' in recapRemakeArgs(p, { length: 56 }, song, none, 42), false);
	// No music stays no music; an unknown song is left to the server.
	assert.equal(recapRemakeArgs(p, { length: 56 }, null, none, 42).soundtrack, 'none');
	assert.equal('soundtrack' in recapRemakeArgs(p, { length: 56 }, undefined, none, 42), false);
	// Another mix: the next vibe picks the song; the rest is kept.
	const mix = recapRemakeArgs(p, { vibe: 'kid_reel' }, song, kept, 42);
	assert.equal(mix.vibe, 'kid_reel');
	assert.equal('soundtrack' in mix, false);
	assert.equal(mix.length, 45);
	assert.deepEqual([mix.exclude, mix.transition], [exclude, transition]);
	// Shuffle: a new draw, kept by later remakes.
	assert.equal(recapRemakeArgs(p, { shuffle: 9 }, song, none, 42).shuffle, 9);
	const shuffled = { ...p, recap: { ...p.recap, shuffle: 9 } };
	assert.equal(recapRemakeArgs(shuffled, { length: 60 }, song, none, 42).shuffle, 9);
	// A custom period, not a year.
	const span = { ...p, recap: { ...p.recap, year: null, from: '2026-03-01', to: '2026-06-30' } };
	const a = recapRemakeArgs(span, {}, song, none, 42);
	assert.equal(a.from, '2026-03-01');
	assert.equal(a.to, '2026-06-30');
	assert.equal('year' in a, false);
	// Not a recap.
	assert.equal(recapRemakeArgs({ ...p, recap: undefined }, {}, song, none, 42), null);
});

test('shorter and longer scale a recap within what the server takes', () => {
	assert.equal(scaledRecapLength(60, 0.75), 45);
	assert.equal(scaledRecapLength(60, 1.25), 75);
	assert.equal(scaledRecapLength(45, 1.25), 56);
	assert.equal(scaledRecapLength(24, 0.75), 20);
	assert.equal(scaledRecapLength(280, 1.25), 300);
});
