<script lang="ts">
	import { appState as s, type PendingFrom } from '$lib/state.svelte';

	// "Stop and apply this instead": a change asked for mid-render (a tweak
	// button, a keyboard shortcut, a song pick) waits here, next to where
	// it was asked for, until the user stops the render for it or keeps
	// rendering. Shared by the tweak panel and the song list.
	let { from }: { from: PendingFrom } = $props();
	const offer = $derived(s.pendingChange?.from === from ? s.pendingChange : null);
</script>

{#if offer}
	<div class="offer" role="status">
		<span>{s.rendering ? 'Still rendering the last change.' : 'The last change is ready.'}</span>
		<button class="btn primary small" type="button" disabled={s.stopping} onclick={() => s.applyPendingChange()}>
			{s.stopping
				? 'Stopping…'
				: `${s.rendering ? 'Stop and apply' : 'Apply'} “${offer.label}”${s.rendering ? ' instead' : ''}`}
		</button>
		<button class="btn ghost small" type="button" disabled={s.stopping} onclick={() => (s.pendingChange = null)}
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
</style>
