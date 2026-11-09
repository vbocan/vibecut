import type { Action } from 'svelte/action';

/**
 * Focus `el`, making it focusable only for this visit when it isn't (a
 * landmark such as `<main>`). A lasting `tabindex` would make every click
 * on its text focus it, and the next Tab would then start over from its
 * top instead of continuing after the click.
 */
export function focusTransient(el: HTMLElement): void {
	const added = !el.hasAttribute('tabindex');
	if (added) el.setAttribute('tabindex', '-1');
	el.focus({ preventScroll: true });
	if (!added) return;
	if (document.activeElement === el) el.addEventListener('blur', () => el.removeAttribute('tabindex'), { once: true });
	else el.removeAttribute('tabindex');
}

/** Where the focus goes back to, given the element that lost it. */
export type FocusFinder = (lost: HTMLElement | null) => HTMLElement | null;

/**
 * Park the focus at the top of `<main>` — or of the open modal dialog the
 * lost element was in, since nothing outside a modal can take the focus —
 * and move it to what `find` returns once that is on the page (straight
 * away when it already is), unless the user has moved the focus elsewhere
 * meanwhile.
 */
function parkUntil(find: () => HTMLElement | null, dialog: HTMLElement | null = null) {
	const park = dialog?.isConnected ? dialog : document.getElementById('main');
	if (!park) return;
	focusTransient(park);
	const ready = find();
	if (ready?.isConnected) {
		ready.focus();
		return;
	}
	const stop = () => {
		observer.disconnect();
		clearTimeout(timer);
	};
	const observer = new MutationObserver(() => {
		if (document.activeElement !== park) return stop();
		const el = find();
		if (!el) return;
		stop();
		el.focus();
	});
	observer.observe(document.body, {
		subtree: true,
		childList: true,
		attributes: true,
		attributeFilter: ['tabindex']
	});
	// A render that never comes back must not leave this watching.
	const timer = setTimeout(stop, 15 * 60_000);
}

/**
 * `use:keepFocus={find}`: when this element leaves the page while it
 * holds the keyboard focus (a re-render clears the timeline and closes
 * the moment panel under it), the focus goes to the top of `<main>`
 * instead of falling onto the page, and on to `find(lost)` once that is
 * back (`lost`: the element inside this one that had the focus).
 */
export const keepFocus: Action<HTMLElement, FocusFinder> = (node, initial) => {
	let find = initial;
	let inside = false;
	let last: HTMLElement | null = null;
	/** The open modal dialog this element is in, seen while it had focus. */
	let dialog: HTMLElement | null = null;
	const onIn = (e: FocusEvent) => {
		inside = true;
		last = e.target as HTMLElement;
		dialog = node.closest<HTMLElement>('dialog[open]');
	};
	const onOut = (e: FocusEvent) => {
		// Without a related target the focus went nowhere: a click on the
		// page, the window losing focus, or this element going away.
		if (e.relatedTarget) inside = node.contains(e.relatedTarget as Node);
		else
			setTimeout(() => {
				if (node.isConnected && !node.contains(document.activeElement)) inside = false;
			});
	};
	node.addEventListener('focusin', onIn);
	node.addEventListener('focusout', onOut);
	return {
		update(next) {
			find = next;
		},
		destroy() {
			node.removeEventListener('focusin', onIn);
			node.removeEventListener('focusout', onOut);
			if (!inside) return;
			const gone = () => !document.activeElement || document.activeElement === document.body;
			const back = () => find(last);
			if (gone()) parkUntil(back, dialog);
			// Still in the document: look again once it has been removed.
			else if (node.contains(document.activeElement))
				queueMicrotask(() => {
					if (gone()) parkUntil(back, dialog);
				});
		}
	};
};
