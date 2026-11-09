<script lang="ts">
	import { appState as s, type PendingFrom } from '$lib/state.svelte';
	import { keepFocus, type FocusFinder } from '$lib/focus';

	// "Stop and apply this instead": a change asked for mid-render (a tweak
	// button, a keyboard shortcut, a song pick) waits here, next to where
	// it was asked for, until the user stops the render for it or keeps
	// rendering. Shared by the tweak panel and the song list.
	//
	// Keyboard and screen readers: the offer is announced through a status
	// line that is always on the page (a live region added together with
	// its text is often not read); the buttons stay focusable while a stop
	// is on its way (aria-disabled, not disabled, so a press doesn't drop
	// the focus onto the page); and when the offer goes away under the
	// focus, the focus goes to `returnFocus()` instead of the page.
	let { from, returnFocus }: { from: PendingFrom; returnFocus?: FocusFinder } = $props();
	const offer = $derived(s.pendingChange?.from === from ? s.pendingChange : null);
	const uid = $props.id();

	const message = $derived(
		offer
			? s.rendering
				? `Still rendering the last change. “${offer.label}” is waiting: stop the render and apply it, or keep rendering.`
				: `The last change is ready. “${offer.label}” is waiting to be applied.`
			: ''
	);
	const fallback: FocusFinder = () => null;

	function apply() {
		if (!s.stopping) void s.applyPendingChange();
	}
	function dismiss() {
		if (!s.stopping) s.pendingChange = null;
	}
	/** Escape on the offer's buttons is "Keep rendering". */
	function onKey(e: KeyboardEvent) {
		if (e.key !== 'Escape') return;
		e.stopPropagation();
		dismiss();
	}
</script>

<p class="sr-only" role="status">{message}</p>
{#if offer}
	<div
		class="offer"
		role="group"
		aria-labelledby="{uid}-what"
		use:keepFocus={returnFocus ?? fallback}
	>
		<span id="{uid}-what">{s.rendering ? 'Still rendering the last change.' : 'The last change is ready.'}</span>
		<button class="btn primary small" type="button" aria-disabled={s.stopping} onclick={apply} onkeydown={onKey}>
			{s.stopping
				? 'Stopping…'
				: `${s.rendering ? 'Stop and apply' : 'Apply'} “${offer.label}”${s.rendering ? ' instead' : ''}`}
		</button>
		<button class="btn ghost small" type="button" aria-disabled={s.stopping} onclick={dismiss} onkeydown={onKey}
			>{s.rendering ? 'Keep rendering' : 'Never mind'}</button
		>
	</div>
{/if}

<style>
	.offer {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 8px;
		padding: 10px 12px;
		border-radius: var(--r-md);
		border: 1px solid var(--line-strong);
		color: var(--text-dim);
		font-size: var(--fs-sm);
	}

	.offer .small {
		padding: 6px 12px;
		font-size: var(--fs-xs);
	}

	.offer [aria-disabled='true'] {
		opacity: 0.6;
		cursor: progress;
	}
</style>
