// Pure rules for changes asked for while a render may be running, shared
// by the app state (requestChange) and the Studio's keyboard shortcuts.
// Kept free of runes and $lib imports so `node --test` can load it.

/** What to do with a change the user just asked for. */
export type ChangeDecision =
	/** Nothing is rendering: run it now. */
	| 'run'
	/** A render is running: offer "Stop and apply this instead". */
	| 'offer'
	/** A Stop is already on its way: say so (a notice), don't keep it. */
	| 'busy';

export function decideChange(state: { stopping: boolean; rendering: boolean }): ChangeDecision {
	if (state.stopping) return 'busy';
	return state.rendering ? 'offer' : 'run';
}

/** The Studio's single-key shortcuts that change the project (the
 *  shortcuts list: shorter, longer, energy, calmer, shuffle, mix, song). */
export const TWEAK_SHORTCUTS: readonly string[] = ['[', ']', 'e', 'c', 's', 'm', 'n'];

/** True for a key that changes the project. Delete / Backspace drop the
 *  selected moment, so they only count with one selected. */
export function isTweakKey(key: string, momentSelected: boolean): boolean {
	if (key === 'Delete' || key === 'Backspace') return momentSelected;
	return TWEAK_SHORTCUTS.includes(key);
}
