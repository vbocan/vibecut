// Unit tests for the radio-group / tablist keyboard model.
// Run with `npm test` (Node's built-in runner; Node ≥ 22.18 strips the
// TypeScript types of the imported module itself, so no extra packages).
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { rovingTarget, tabStopIndex } from '../src/lib/radiogroup.ts';

const all = (n) => Array(n).fill(true);

test('arrow keys move to the next / previous item and wrap around', () => {
	assert.equal(rovingTarget('ArrowRight', 0, all(4)), 1);
	assert.equal(rovingTarget('ArrowDown', 2, all(4)), 3);
	assert.equal(rovingTarget('ArrowRight', 3, all(4)), 0, 'last → first');
	assert.equal(rovingTarget('ArrowLeft', 2, all(4)), 1);
	assert.equal(rovingTarget('ArrowUp', 1, all(4)), 0);
	assert.equal(rovingTarget('ArrowLeft', 0, all(4)), 3, 'first → last');
});

test('Home and End go to the first and last item', () => {
	assert.equal(rovingTarget('Home', 2, all(5)), 0);
	assert.equal(rovingTarget('End', 1, all(5)), 4);
});

test('disabled items are skipped, including across the wrap', () => {
	const en = [true, false, true, false];
	assert.equal(rovingTarget('ArrowRight', 0, en), 2);
	assert.equal(rovingTarget('ArrowRight', 2, en), 0);
	assert.equal(rovingTarget('ArrowLeft', 0, en), 2);
	assert.equal(rovingTarget('Home', 3, [false, false, true, true]), 2);
	assert.equal(rovingTarget('End', 0, [true, true, false, false]), 1);
});

test('from no focused item, next goes to the first and previous to the last', () => {
	assert.equal(rovingTarget('ArrowDown', -1, all(3)), 0);
	assert.equal(rovingTarget('ArrowUp', -1, all(3)), 2);
});

test('orientation limits which arrows navigate', () => {
	assert.equal(rovingTarget('ArrowDown', 0, all(3), 'horizontal'), null);
	assert.equal(rovingTarget('ArrowRight', 0, all(3), 'horizontal'), 1);
	assert.equal(rovingTarget('ArrowRight', 0, all(3), 'vertical'), null);
	assert.equal(rovingTarget('ArrowUp', 1, all(3), 'vertical'), 0);
});

test('other keys and empty or fully disabled groups are ignored', () => {
	assert.equal(rovingTarget('a', 0, all(3)), null);
	assert.equal(rovingTarget(' ', 0, all(3)), null);
	assert.equal(rovingTarget('Tab', 0, all(3)), null);
	assert.equal(rovingTarget('ArrowRight', 0, []), null);
	assert.equal(rovingTarget('ArrowRight', 0, [false, false]), null);
});

test('a single item stays put', () => {
	assert.equal(rovingTarget('ArrowRight', 0, [true]), 0);
	assert.equal(rovingTarget('ArrowLeft', 0, [true]), 0);
});

test('the tab stop is the checked item, else the first enabled one', () => {
	assert.equal(tabStopIndex([false, false, true], all(3)), 2);
	assert.equal(tabStopIndex([false, false, false], all(3)), 0);
	assert.equal(tabStopIndex([false, false, false], [false, true, true]), 1);
	assert.equal(tabStopIndex([true, false], [false, true]), 1, 'a disabled checked item is not the stop');
	assert.equal(tabStopIndex([], []), -1);
	assert.equal(tabStopIndex([false], [false]), -1);
});
