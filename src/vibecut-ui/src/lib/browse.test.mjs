// Tests for the drive-browsing helpers in browse.ts.
// Run from src/vibecut-ui:  npm test  (or node --test src/lib/browse.test.mjs)

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { addRecent, folderName, footageValue, MAX_RECENTS, moveHighlight, parseRecents, videoCount } from './browse.ts';

const recent = (folder, over = {}) => ({ folder, path: folder, name: folderName(folder), sample_video: null, ...over });

test('footageValue: folders in /footage use the trip list value, others their path', () => {
	assert.equal(footageValue('/footage'), '');
	assert.equal(footageValue('/footage/'), '');
	assert.equal(footageValue('/footage/Alps Ski Day'), 'Alps Ski Day');
	assert.equal(footageValue('/footage/trips/alps'), 'trips/alps');
	assert.equal(footageValue('/footagex/a'), '/footagex/a');
	assert.equal(footageValue('/host/d/GoPro/Alps'), '/host/d/GoPro/Alps');
});

test('folderName: last part of any path style', () => {
	assert.equal(folderName('/host/d/GoPro/Alps'), 'Alps');
	assert.equal(folderName('D:\\GoPro\\Coastline Bike Ride\\'), 'Coastline Bike Ride');
	assert.equal(folderName('trips/alps'), 'alps');
	assert.equal(folderName(''), 'All footage');
});

test('addRecent: newest first, no duplicates, capped', () => {
	let list = [];
	for (const f of ['/a', '/b', '/c', '/d', '/e', '/f']) list = addRecent(list, recent(f));
	assert.deepEqual(
		list.map((r) => r.folder),
		['/f', '/e', '/d', '/c', '/b']
	);
	assert.equal(list.length, MAX_RECENTS);
	list = addRecent(list, recent('/c', { name: 'again' }));
	assert.deepEqual(
		list.map((r) => r.folder),
		['/c', '/f', '/e', '/d', '/b']
	);
	assert.equal(list[0].name, 'again');
});

test('parseRecents: tolerates junk', () => {
	assert.deepEqual(parseRecents(null), []);
	assert.deepEqual(parseRecents('not json'), []);
	assert.deepEqual(parseRecents('{"a":1}'), []);
	const raw = JSON.stringify([
		recent('/a', { sample_video: '/a/GX010001.MP4' }),
		{ folder: 3 },
		null,
		recent('/a'),
		{ folder: '/b', path: 'B:\\', name: 'b', sample_video: 7 },
		...['/c', '/d', '/e', '/f'].map((f) => recent(f))
	]);
	const got = parseRecents(raw);
	assert.deepEqual(
		got.map((r) => r.folder),
		['/a', '/b', '/c', '/d', '/e']
	);
	assert.equal(got[0].sample_video, '/a/GX010001.MP4');
	assert.equal(got[1].sample_video, null);
});

test('moveHighlight: arrows, Home/End, pages, clamped', () => {
	assert.equal(moveHighlight('ArrowDown', 0, 3), 1);
	assert.equal(moveHighlight('ArrowDown', 2, 3), 2);
	assert.equal(moveHighlight('ArrowUp', 0, 3), 0);
	assert.equal(moveHighlight('ArrowUp', -1, 3), 0);
	assert.equal(moveHighlight('Home', 2, 3), 0);
	assert.equal(moveHighlight('End', 0, 3), 2);
	assert.equal(moveHighlight('PageDown', 0, 20), 8);
	assert.equal(moveHighlight('PageUp', 3, 20), 0);
	assert.equal(moveHighlight('Enter', 0, 3), null);
	assert.equal(moveHighlight('ArrowDown', 0, 0), null);
});

test('videoCount', () => {
	assert.equal(videoCount(0), '');
	assert.equal(videoCount(1), '1 video');
	assert.equal(videoCount(12), '12 videos');
	assert.equal(videoCount(12, false), '12+ videos');
	assert.equal(videoCount(0, false), '…');
});
