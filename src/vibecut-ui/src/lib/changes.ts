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

/** How "Finish in full quality" goes for the draft on screen. */
export type FinishDecision =
	/** The edit hasn't changed since the draft: finish it, same version. */
	| 'finish'
	/** It has (unsaved edits, or a saved edit since the draft): ask. */
	| 'ask'
	/** Put the draft's own config back (dropping the edits), then finish it. */
	| 'restore_and_finish'
	/** Render the project as it is now as a new version, not as the draft. */
	| 'new_version';

/**
 * `known`: the draft's config text is known here (a version rendered in
 * this session; one opened from the Renders list isn't). `unsaved`: the
 * editors hold unsaved edits. `savedSince`: the config on disk differs
 * from the draft's. `choice`: the user's answer to the ask, if any.
 * A saved edit is never overwritten without asking, and edits are never
 * folded into a finish of the draft.
 */
export function decideFinish(state: {
	known: boolean;
	unsaved: boolean;
	savedSince: boolean;
	choice?: 'draft' | 'edits';
}): FinishDecision {
	if (!state.known || state.choice === 'edits') return 'new_version';
	const changed = state.unsaved || state.savedSince;
	if (!changed) return 'finish';
	return state.choice === 'draft' ? 'restore_and_finish' : 'ask';
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
