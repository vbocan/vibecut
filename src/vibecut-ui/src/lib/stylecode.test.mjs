// Tests for stylecode.ts.
// Run from src/vibecut-ui:  npm test  (or node --test src/lib/stylecode.test.mjs)

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { extractStyleCode, looksLikeAnAttempt, musicTone } from './stylecode.ts';

const CODE = 'VC1-gwGhssPU5faoChVGdWxsIFNlbmQgTmlnaHQgRHJpdmUAAgABEqwCIy25';

test('a pasted code is found as is, quoted, wrapped over lines or inside a message', () => {
	assert.equal(extractStyleCode(CODE), CODE);
	assert.equal(extractStyleCode(`  “${CODE}” `), CODE);
	assert.equal(extractStyleCode(`${CODE.slice(0, 25)}\n${CODE.slice(25)}`), CODE);
	assert.equal(extractStyleCode(`try my style: ${CODE} 🔥`), CODE);
	assert.equal(extractStyleCode(`(${CODE})`), CODE);
	assert.equal(extractStyleCode('VC1-Q1VCGg'), 'VC1-Q1VCGg', 'the shortest codes are 10 characters');
	assert.equal(extractStyleCode('vc1-Q1VCGg'), 'VC1-Q1VCGg', 'the prefix is upper-cased; the rest is kept');
	assert.equal(extractStyleCode('VC2-Q1VCGgAA'), 'VC2-Q1VCGgAA', 'a newer format is passed on, for the server to explain');
});

test('text without a code is not mistaken for one', () => {
	for (const t of ['', '   ', 'hello', 'VC1-', 'VC1-abc', 'ABCVC1-Q1VCGg', 'VC-Q1VCGgxx', 'a 30 second action reel']) {
		assert.equal(extractStyleCode(t), null, t);
	}
	assert.equal(looksLikeAnAttempt('VC1-abc'), true);
	assert.equal(looksLikeAnAttempt(''), false);
	assert.equal(looksLikeAnAttempt(CODE), false);
});

test('the song a style gets reads as good news only when it is the style’s own', () => {
	assert.equal(musicTone('same_song'), 'good');
	assert.equal(musicTone('same_title'), 'good');
	assert.equal(musicTone('similar_tempo'), 'warn');
	assert.equal(musicTone('vibecut_pick'), 'warn');
	assert.equal(musicTone('no_music'), 'warn');
	assert.equal(musicTone('original_sound'), 'plain');
});
