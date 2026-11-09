// Tests for the download helpers in download.ts.
// Run from src/vibecut-ui:  node --test src/lib/download.test.mjs

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { fileNameFromDisposition } from './download.ts';

test('the exact UTF-8 name wins over the ASCII fallback', () => {
	const h = `attachment; filename="_t_ reel.fcpxml"; filename*=UTF-8''%C3%89t%C3%A9%20reel.fcpxml`;
	assert.equal(fileNameFromDisposition(h, 'x'), 'Été reel.fcpxml');
});

test('a plain filename, quoted or not', () => {
	assert.equal(fileNameFromDisposition('attachment; filename="coast.otio"', 'x'), 'coast.otio');
	assert.equal(fileNameFromDisposition('attachment; filename=coast.otio', 'x'), 'coast.otio');
});

test('no usable name falls back', () => {
	assert.equal(fileNameFromDisposition(null, 'timeline.otio'), 'timeline.otio');
	assert.equal(fileNameFromDisposition('attachment', 'timeline.otio'), 'timeline.otio');
	// Broken percent-encoding: the plain name, else the fallback.
	assert.equal(fileNameFromDisposition(`attachment; filename="a.otio"; filename*=UTF-8''%E0%A4%A`, 'x'), 'a.otio');
	assert.equal(fileNameFromDisposition(`attachment; filename*=UTF-8''%E0%A4%A`, 'x'), 'x');
});
