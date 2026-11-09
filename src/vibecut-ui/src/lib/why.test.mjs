// Tests for the moment inspector's "why this moment" words in why.ts.
// Run from src/vibecut-ui:  npm test  (or node --test src/lib/why.test.mjs)

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { whyChips, whySentence } from './why.ts';

const sig = (kind, share, label) => ({ kind, share, label });

test('chips keep the picker’s order and say how much each signal counted', () => {
	const chips = whyChips([
		sig('landing', 0.61, 'airtime 0.6 s'),
		sig('accel', 0.26, '3.0 g jolt'),
		sig('scene_start', 0, 'opens on a scene change')
	]);
	assert.deepEqual(
		chips.map((c) => c.label),
		['airtime 0.6 s', '3.0 g jolt', 'opens on a scene change']
	);
	assert.equal(chips[0].detail, '61% of this moment’s score');
	assert.equal(chips[2].detail, 'where the clip starts');
	assert.equal(chips[0].kind, 'landing');
});

test('older manifests and unscored moments have no chips and no sentence', () => {
	for (const none of [undefined, null, []]) {
		assert.deepEqual(whyChips(none), []);
		assert.equal(whySentence(none), null);
	}
	assert.deepEqual(whyChips([sig('motion', 1, ' ')]), []);
	// A placement note alone isn't a reason.
	assert.equal(whySentence([sig('scene_start', 0, 'opens on a scene change')]), null);
});

test('the sentence follows the strongest measured reason', () => {
	assert.match(whySentence([sig('landing', 0.7, 'airtime 0.6 s'), sig('accel', 0.3, '3.0 g jolt')]), /jump/);
	assert.match(whySentence([sig('hilight', 0.9, 'HiLight tag')]), /HiLight/);
	assert.match(whySentence([sig('gyro', 0.6, 'fast spin (458°/s)')]), /spun/);
	assert.match(whySentence([sig('accel', 0.6, '2.4 g jolt')]), /jolt/);
	assert.match(whySentence([sig('gps_speed', 0.6, '52 km/h, top speed')]), /fastest/);
	assert.match(whySentence([sig('gps_speed', 0.6, '38 km/h')]), /fast stretch/);
	assert.match(whySentence([sig('gps_speed', 0.6, 'hard stop from 31 km/h')]), /hard stop/i);
	assert.match(whySentence([sig('gps_speed', 0.6, 'hard braking from 40 km/h')]), /brakes/);
	// On-screen motion keeps the generic, rank-based wording.
	assert.equal(whySentence([sig('motion', 1, 'lots of movement on screen')]), null);
});
