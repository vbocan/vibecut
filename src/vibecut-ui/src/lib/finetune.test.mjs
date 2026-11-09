// Tests for finetune.ts.
// Run from src/vibecut-ui:  npm test  (or node --test src/lib/finetune.test.mjs)

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { CONTROLS, GROUPS, formatDuration, getPath, keyLabel, parseDuration, sameValue, showValue } from './finetune.ts';

test('every control has a unique dotted key, a label and help', () => {
	const keys = GROUPS.flatMap((g) => g.controls.map((c) => c.key));
	assert.equal(new Set(keys).size, keys.length);
	for (const g of GROUPS)
		for (const c of g.controls) {
			assert.match(c.key, /^[a-z_]+(\.[a-z0-9_]+)+$/, c.key);
			assert.ok(c.label && c.help.length > 10, c.key);
			if (c.kind === 'choice') assert.ok(c.choices && c.choices.length >= 2, c.key);
			if (c.kind === 'range' || c.kind === 'seconds') assert.ok(c.min !== undefined && c.max > c.min && c.step > 0, c.key);
		}
});

test('durations read and write the way the config spells them', () => {
	assert.equal(parseDuration('7s'), 7);
	assert.equal(parseDuration('700ms'), 0.7);
	assert.equal(parseDuration('1s 500ms'), 1.5);
	assert.equal(parseDuration('1m30s'), 90);
	assert.equal(parseDuration('2.5s'), 2.5);
	assert.ok(Number.isNaN(parseDuration('soon')));
	for (const s of [0, 0.35, 0.7, 1.5, 3, 7, 59, 60, 90, 300]) assert.equal(parseDuration(formatDuration(s)), s, `${s}`);
	assert.equal(formatDuration(7), '7s');
	assert.equal(formatDuration(0.7), '700ms');
	assert.equal(formatDuration(300), '5m');
});

test('values read as people say them', () => {
	assert.equal(showValue(CONTROLS.get('soundtrack.ducking_db'), -16), '−16 dB');
	assert.equal(showValue(CONTROLS.get('selection.variety'), 0.05), '5%');
	assert.equal(showValue(CONTROLS.get('selection.max_clip'), '7s'), '7 s');
	assert.equal(showValue(CONTROLS.get('project.target_duration'), '1m30s'), '1 min 30 s');
	assert.equal(showValue(CONTROLS.get('project.target_duration'), '2m'), '2 min');
	assert.equal(showValue(CONTROLS.get('selection.order'), 'energy_arc'), 'Build to the best moment');
	assert.equal(showValue(CONTROLS.get('outputs.0.crf'), 20), 'Better');
	assert.equal(showValue(CONTROLS.get('grading.stabilize'), false), 'Off');
});

test('changes compare durations by length and numbers loosely', () => {
	const c = CONTROLS.get('selection.max_clip');
	assert.ok(sameValue(c, '7s', '7000ms'));
	assert.ok(!sameValue(c, '7s', '5s'));
	assert.ok(sameValue(CONTROLS.get('scoring.gps_speed'), 0.4, 0.4000000059604645));
	assert.ok(!sameValue(undefined, 1, undefined));
});

test('paths and labels', () => {
	const cfg = { intro: { layers: [{ content: 'Hi' }] }, selection: { max_clip: '7s' } };
	assert.equal(getPath(cfg, 'intro.layers.0.content'), 'Hi');
	assert.equal(getPath(cfg, 'outro.layers.0.content'), undefined);
	assert.equal(keyLabel('selection.max_clip'), 'Longest shot');
	assert.equal(keyLabel('selection.min_clip_extra'), 'Selection · min clip extra');
	assert.equal(keyLabel('intro.layers.0.size'), 'Intro · layers · size');
});
