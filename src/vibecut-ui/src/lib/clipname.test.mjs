// Tests for clipname.ts.
// Run from src/vibecut-ui:  npm test  (or node --test src/lib/clipname.test.mjs)

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { shortClipName } from './clipname.ts';

test('short ids stay whole', () => {
	assert.equal(shortClipName('0301'), '0301');
	assert.equal(shortClipName('GX010301'), 'GX010301');
	assert.equal(shortClipName('a'.repeat(15)), 'a'.repeat(15));
});

test('long ids keep their start and their distinguishing end', () => {
	const s = shortClipName('DJI_20261010075256_0003_D');
	assert.equal(s, 'DJI_…256_0003_D');
	assert.equal([...s].length, 15);
	assert.notEqual(shortClipName('DJI_20261010075438_0004_D'), s);
});

test('the limit is in characters, not UTF-16 units', () => {
	const s = shortClipName('Plajă_la_Vama_Veche_2026', 10);
	assert.equal([...s].length, 10);
	assert.ok(s.startsWith('Pla') && s.endsWith('2026'), s);
});
