// Tests for the music download sites in music.ts.
// Run from src/vibecut-ui:  npm test  (or node --test src/lib/musicsources.test.mjs)

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { MUSIC_SOURCES } from './music.ts';

test('every music source is a distinct https link with its terms', () => {
	assert.ok(MUSIC_SOURCES.length >= 4);
	const urls = new Set();
	for (const s of MUSIC_SOURCES) {
		assert.match(s.url, /^https:\/\/[a-z0-9.-]+\//, s.name);
		assert.ok(!urls.has(s.url), `${s.url} twice`);
		urls.add(s.url);
		assert.ok(s.name && s.terms.length > 10 && s.badge, s.name);
		assert.ok(['ok', 'credit', 'caution'].includes(s.tone), s.name);
	}
});
