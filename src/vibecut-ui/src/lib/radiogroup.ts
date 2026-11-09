import type { Action } from 'svelte/action';

/**
 * Keyboard model for single-choice groups — the WAI-ARIA radio group
 * pattern, and tabs with automatic activation (the same model on
 * different attributes):
 *
 *  - one tab stop per group: the checked item, or the first enabled one
 *    when nothing is checked (roving tabindex);
 *  - Arrow keys move to the next / previous enabled item, wrapping
 *    around, and select it; Home / End go to the first / last;
 *  - Space selects the focused item.
 *
 * Selecting goes through the item's own click handler, so a component
 * keeps one code path for pointer and keyboard. The pure helpers below
 * hold the logic and are unit-tested (test/radiogroup.test.mjs).
 */

export type Orientation = 'both' | 'horizontal' | 'vertical';

const NEXT: Record<Orientation, string[]> = {
	both: ['ArrowRight', 'ArrowDown'],
	horizontal: ['ArrowRight'],
	vertical: ['ArrowDown']
};
const PREV: Record<Orientation, string[]> = {
	both: ['ArrowLeft', 'ArrowUp'],
	horizontal: ['ArrowLeft'],
	vertical: ['ArrowUp']
};

/**
 * Index `key` moves to from item `current` (−1 when no item has focus),
 * skipping disabled items and wrapping around. Null when the key isn't
 * a navigation key for this orientation, or nothing is enabled.
 */
export function rovingTarget(
	key: string,
	current: number,
	enabled: readonly boolean[],
	orientation: Orientation = 'both'
): number | null {
	const n = enabled.length;
	if (!enabled.some(Boolean)) return null;
	let start: number;
	let step: number;
	if (key === 'Home') [start, step] = [0, 1];
	else if (key === 'End') [start, step] = [n - 1, -1];
	else if (NEXT[orientation].includes(key)) [start, step] = [current + 1, 1];
	else if (PREV[orientation].includes(key)) [start, step] = [(current < 0 ? n : current) - 1, -1];
	else return null;
	for (let k = 0; k < n; k++) {
		const i = (((start + k * step) % n) + n) % n;
		if (enabled[i]) return i;
	}
	return null;
}

/**
 * The group's single tab stop: the checked item when it is enabled,
 * else the first enabled item; −1 when nothing is enabled.
 */
export function tabStopIndex(checked: readonly boolean[], enabled: readonly boolean[]): number {
	const c = checked.findIndex((x, i) => x && enabled[i]);
	return c >= 0 ? c : enabled.findIndex(Boolean);
}

export interface RovingOptions {
	/** Item selector. Default `[role="radio"]`, or `[role="tab"]` in a tablist. */
	items?: string;
	/** Default `both` for radio groups, `horizontal` for tablists. */
	orientation?: Orientation;
	/**
	 * `false`: arrows only move focus and Space / Enter selects. For
	 * groups where choosing starts expensive work (a re-render), as
	 * the APG recommends for tabs with manual activation. Default true.
	 */
	selectOnMove?: boolean;
}

/**
 * `use:radiogroup` on an element with `role="radiogroup"` (items with
 * `role="radio"` + `aria-checked`) or `role="tablist"` (items with
 * `role="tab"` + `aria-selected`). Items of a nested group of the same
 * role belong to that group, not this one.
 */
export const radiogroup: Action<HTMLElement, RovingOptions | undefined> = (node, initial) => {
	let opts: RovingOptions = initial ?? {};
	const role = node.getAttribute('role') ?? 'radiogroup';
	const tabs = role === 'tablist';
	const stateAttr = tabs ? 'aria-selected' : 'aria-checked';
	const selector = () => opts.items ?? (tabs ? '[role="tab"]' : '[role="radio"]');

	const items = () =>
		Array.from(node.querySelectorAll<HTMLElement>(selector())).filter(
			(el) => el.parentElement?.closest(`[role="${role}"]`) === node
		);
	const enabled = (el: HTMLElement) =>
		!(el as HTMLButtonElement).disabled && el.getAttribute('aria-disabled') !== 'true';
	const checked = (el: HTMLElement) => el.getAttribute(stateAttr) === 'true';

	function sync() {
		const list = items();
		const stop = tabStopIndex(list.map(checked), list.map(enabled));
		list.forEach((el, i) => {
			const want = i === stop ? '0' : '-1';
			if (el.getAttribute('tabindex') !== want) el.setAttribute('tabindex', want);
		});
	}

	function onKeydown(e: KeyboardEvent) {
		if (e.altKey || e.ctrlKey || e.metaKey) return;
		const item = (e.target as HTMLElement | null)?.closest<HTMLElement>(selector());
		const list = items();
		const i = item ? list.indexOf(item) : -1;
		if (!item || i < 0) return;
		// Buttons already click on Space; anything else needs it done.
		if (e.key === ' ' && !(item instanceof HTMLButtonElement)) {
			e.preventDefault();
			if (enabled(item)) item.click();
			return;
		}
		const next = rovingTarget(e.key, i, list.map(enabled), opts.orientation ?? (tabs ? 'horizontal' : 'both'));
		if (next === null) return;
		e.preventDefault();
		const target = list[next];
		target.focus();
		if (opts.selectOnMove !== false && !checked(target)) target.click();
	}

	// Svelte updates aria-checked / the item list; keep the tab stop on
	// whatever is checked now.
	const observer = new MutationObserver(sync);
	observer.observe(node, {
		subtree: true,
		childList: true,
		attributes: true,
		attributeFilter: [stateAttr, 'disabled', 'aria-disabled']
	});
	node.addEventListener('keydown', onKeydown);
	sync();

	return {
		update(next) {
			opts = next ?? {};
			sync();
		},
		destroy() {
			observer.disconnect();
			node.removeEventListener('keydown', onKeydown);
		}
	};
};
