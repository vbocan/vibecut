// Tests for the mid-render change rules in changes.ts.
// Run from src/vibecut-ui:  npm test  (or node --test src/lib/changes.test.mjs)

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { decideChange, decideFinish, isTweakKey, TWEAK_SHORTCUTS } from './changes.ts';

test('a change mid-render is offered, not ignored; idle runs it; a pending stop says so', () => {
	assert.equal(decideChange({ stopping: false, rendering: false }), 'run');
	assert.equal(decideChange({ stopping: false, rendering: true }), 'offer');
	assert.equal(decideChange({ stopping: true, rendering: true }), 'busy');
	assert.equal(decideChange({ stopping: true, rendering: false }), 'busy');
});

test('every tweak shortcut counts as a change; playback and navigation keys do not', () => {
	for (const k of TWEAK_SHORTCUTS) assert.ok(isTweakKey(k, false), k);
	for (const k of [' ', 'k', 'j', 'l', 'd', '?', 'ArrowLeft', 'ArrowRight', 'Escape', 'x']) {
		assert.equal(isTweakKey(k, true), false, k);
	}
});

test('Delete and Backspace are a change only with a moment selected', () => {
	assert.equal(isTweakKey('Delete', true), true);
	assert.equal(isTweakKey('Backspace', true), true);
	assert.equal(isTweakKey('Delete', false), false);
	assert.equal(isTweakKey('Backspace', false), false);
});

test('finishing a draft never overwrites a saved edit or folds edits in without asking', () => {
	const base = { known: true, unsaved: false, savedSince: false };
	assert.equal(decideFinish(base), 'finish');
	// A saved Guided edit since the draft: ask, don't write the draft over it.
	assert.equal(decideFinish({ ...base, savedSince: true }), 'ask');
	// Unsaved edits: ask, don't render them and call it the draft.
	assert.equal(decideFinish({ ...base, unsaved: true }), 'ask');
	assert.equal(decideFinish({ ...base, unsaved: true, savedSince: true }), 'ask');
	// The answers.
	assert.equal(decideFinish({ ...base, savedSince: true, choice: 'draft' }), 'restore_and_finish');
	assert.equal(decideFinish({ ...base, unsaved: true, choice: 'edits' }), 'new_version');
	// A draft whose config isn't known (opened from the Renders list) is
	// never finished as itself.
	assert.equal(decideFinish({ known: false, unsaved: false, savedSince: false }), 'new_version');
	assert.equal(decideFinish({ known: false, unsaved: true, savedSince: false, choice: 'draft' }), 'new_version');
});
