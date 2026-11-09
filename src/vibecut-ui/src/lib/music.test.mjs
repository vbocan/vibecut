// Tests for the pure song / credit helpers in music.ts.
// Run from src/vibecut-ui:  node --test src/lib/music.test.mjs
// (Node 22.18+ / 23.6+ strip music.ts's types natively; no packages needed.)

import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
	captionMusicLines,
	labelsFollowProject,
	licenceBadge,
	licenceKindFromId,
	musicCaptionLine,
	projectSongPath,
	titleFromPath,
	trackForPath
} from './music.ts';

const fullSend = {
	path: '/projects/music/Full_Send_Night_Drive.mp3',
	title: 'Full Send Night Drive',
	artist: 'Demo Composer',
	credit: 'Music: "Full Send Night Drive" by Demo Composer — licensed under CC BY 4.0',
	attribution_required: true
};
const trail = { ...trackForPath('/projects/music/Trail_Running_Pulse.mp3', []), artist: null };

test('on the project, labels follow its config — not a fresh pick', () => {
	// Studio after a reload: versions exist, whatever the make step says.
	for (const makeStep of ['idle', 'done', 'planning']) {
		assert.equal(labelsFollowProject({ hasProposal: true, versions: 2, makeStep, makeError: null }), true, makeStep);
	}
	// The first render of a proposal: running, finished, failed.
	for (const makeStep of ['rendering', 'done', 'failed']) {
		assert.equal(labelsFollowProject({ hasProposal: true, versions: 0, makeStep, makeError: null }), true, makeStep);
	}
	// Create screen, and "Make my video" before its proposal exists: the next video.
	for (const makeStep of ['idle', 'scanning', 'planning']) {
		assert.equal(labelsFollowProject({ hasProposal: true, versions: 0, makeStep, makeError: null }), false, makeStep);
	}
	// The proposal step failed: the older proposal isn't what was attempted.
	assert.equal(labelsFollowProject({ hasProposal: true, versions: 0, makeStep: 'failed', makeError: 'boom' }), false);
	assert.equal(labelsFollowProject({ hasProposal: false, versions: 0, makeStep: 'done', makeError: null }), false);
});

test("the project's song is its config's file, else the proposal's record", () => {
	const proposalSong = { path: fullSend.path };
	assert.equal(projectSongPath({ configLoaded: true, configFiles: [fullSend.path, '/b.mp3'], proposalSong: null }), fullSend.path);
	assert.equal(projectSongPath({ configLoaded: true, configFiles: undefined, proposalSong }), null, 'no [soundtrack]: no music');
	assert.equal(projectSongPath({ configLoaded: false, configFiles: undefined, proposalSong }), fullSend.path);
	assert.equal(projectSongPath({ configLoaded: false, configFiles: undefined, proposalSong: null }), null);
	assert.equal(projectSongPath({ configLoaded: false, configFiles: undefined, proposalSong: undefined }), undefined);
});

test('a song missing from the library list keeps its known title and credit', () => {
	const lib = [trackForPath(fullSend.path, [], { title: 'In library' })];
	assert.equal(trackForPath(fullSend.path, lib), lib[0]);
	const credit = {
		file: '/x/night.mp3',
		title: 'Night',
		artist: 'Ada',
		licence: 'CC-BY-4.0',
		licence_label: 'CC BY 4.0',
		source_url: null,
		credit: 'Night by Ada (CC BY 4.0)',
		attribution_required: true,
		sha256: null,
		size_bytes: 1
	};
	const t = trackForPath('/x/night.mp3', lib, { credit, title: 'ignored' });
	assert.deepEqual([t.title, t.artist, t.credit, t.attribution_required, t.licence.kind], ['Night', 'Ada', credit.credit, true, 'cc_by']);
	assert.equal(trackForPath('/x/night.mp3', [], { title: 'From the proposal' }).title, 'From the proposal');
	assert.equal(trackForPath('/x/summer_drive-124bpm.mp3', []).title, 'Summer drive 124bpm');
	assert.equal(titleFromPath('/x/__.wav'), 'Untitled');
});

test("the caption credits the render's own soundtrack record first", () => {
	// The reviewers' case: the video has Full Send (CC BY); a fresh pick would be Trail.
	assert.deepEqual(captionMusicLines([fullSend], trail), [fullSend.credit]);
	// Every recorded file is credited.
	assert.deepEqual(captionMusicLines([fullSend, trail], null), [fullSend.credit, 'Music: Trail Running Pulse']);
	// No record (older manifest, not loaded yet): the project's song.
	assert.deepEqual(captionMusicLines(undefined, fullSend), [fullSend.credit]);
	assert.deepEqual(captionMusicLines([], null), []);
	assert.equal(musicCaptionLine({ ...fullSend, attribution_required: false }), 'Music: Full Send Night Drive by Demo Composer');
});

test('licence ids map to kinds and badges', () => {
	const kinds = ['CC0-1.0', 'CC-BY-4.0', 'CC-BY-SA-4.0', 'CC-BY-NC-SA-4.0', 'CC-BY-ND-4.0', 'Pixabay', 'YouTube-Audio-Library', 'unknown', 'Free with attribution'];
	assert.deepEqual(kinds.map(licenceKindFromId), ['cc0', 'cc_by', 'cc_by_sa', 'cc_by_nc', 'cc_by_nd', 'pixabay', 'youtube_audio_library', 'unknown', 'custom']);
	const custom = (attribution_required) => ({ ...trail, attribution_required, licence: { id: 'x', label: 'Free with attribution', kind: 'custom', note: '' } });
	assert.equal(licenceBadge(custom(true)).tone, 'credit');
	assert.equal(licenceBadge(custom(false)).tone, 'caution');
});
