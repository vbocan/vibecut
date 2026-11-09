// Tests for titles.ts.
// Run from src/vibecut-ui:  npm test  (or node --test src/lib/titles.test.mjs)

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { ANCHOR_GRID, colorInputValue, cssColor, fontCss, layerOpacity, newCard, newTextLine, placement, placementStyle } from './titles.ts';

test('anchors match the renderer: 7 of them, 20 px margins, 40 px under bottom text', () => {
	assert.equal(ANCHOR_GRID.filter(Boolean).length, 7);
	assert.deepEqual(placement('top-left', 0, 0), { left: 20, top: 20, centerX: false, centerY: false });
	assert.deepEqual(placement('bottom-center', 0, 0).bottom, 40);
	assert.deepEqual(placement('bottom-center', 0, 0, 'image').bottom, 20);
	assert.deepEqual(placement('bottom-left', 0, 0).bottom, 20);
	assert.equal(placement('nonsense', 0, 0).centerY, true);
	// Offsets move right and down, whichever edge the anchor is on.
	assert.equal(placement('top-right', 30, 10).right, -10);
	assert.equal(placement('bottom-right', 0, -60).bottom, 100);
	assert.equal(placement('center', 0, 120).top, 120);
});

test('placement styles scale to the preview', () => {
	const st = placementStyle(placement('center', 100, -50), 0.5);
	assert.match(st, /left: calc\(50% \+ 50\.00px\)/);
	assert.match(st, /top: calc\(50% \+ -25\.00px\)/);
	assert.match(st, /translate\(-50%, -50%\)/);
	assert.match(placementStyle(placement('top-right', 0, 0), 0.5), /right: 10\.00px; top: 10\.00px/);
});

test('layers show in their window and fade at its edges', () => {
	const card = { duration: '4s', fade_in: '1s', fade_out: '1s' };
	assert.equal(layerOpacity({}, card, 0), 0);
	assert.equal(layerOpacity({}, card, 0.5), 0.5);
	assert.equal(layerOpacity({}, card, 2), 1);
	assert.equal(layerOpacity({}, card, 3.5), 0.5);
	const staged = { appear_at: '2s', disappear_at: '3s', fade_in: '0s', fade_out: '0s' };
	assert.equal(layerOpacity(staged, card, 1.9), 0);
	assert.equal(layerOpacity(staged, card, 2.5), 1);
	assert.equal(layerOpacity(staged, card, 3.1), 0);
});

test('fonts and colours for the preview and the inputs', () => {
	assert.match(fontCss('Bebas Neue').family, /Bebas Neue/);
	assert.equal(fontCss('Inter Display:bold').weight, 800);
	assert.match(fontCss('Montserrat-Black.otf').family, /Montserrat Black/);
	assert.equal(colorInputValue('#F5E6C8'), '#f5e6c8');
	assert.equal(colorInputValue('#000000AA'), '#000000');
	assert.equal(colorInputValue('white'), '#ffffff');
	assert.equal(colorInputValue('salmon', '#123456'), '#123456');
	assert.equal(cssColor('#000000AA'), '#000000AA');
	assert.match(cssColor('black@0.5'), /color-mix\(in srgb, black 50%/);
	assert.equal(cssColor('url(x)', '#fff'), '#fff');
});

test('new lines go under the last one; new cards use the vibe or a plain card', () => {
	const l = newTextLine({ size: 90, y_offset: 0, position: 'center', color: '#f5e6c8' });
	assert.equal(l.position, 'center');
	assert.equal(l.color, '#f5e6c8');
	assert.ok(l.y_offset > 60 && l.size < 90, JSON.stringify(l));
	assert.equal(newTextLine(null).y_offset, 0);
	const plain = newCard('outro', 'Alps');
	assert.equal(plain.layers[0].content, 'Thanks for watching');
	const vibe = { duration: '5s', fade_in: '1s', fade_out: '1s', mode: 'card', bg_color: '#000', layers: [{ kind: 'text', content: '{title}' }, { kind: 'image', path: 'x.png' }] };
	const v = newCard('intro', 'Alps', vibe);
	assert.equal(v.layers.length, 1);
	assert.equal(v.layers[0].content, 'Alps');
	assert.equal(vibe.layers[0].content, '{title}', 'the vibe card is not changed');
});
