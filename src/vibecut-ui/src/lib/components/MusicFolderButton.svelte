<script lang="ts">
	import * as api from '$lib/api';
	import { appState as s } from '$lib/state.svelte';
	import BrowseDialog from './BrowseDialog.svelte';

	// "Pick a music folder…": choose a folder of songs on any drive; it joins
	// the music library (the server remembers it) and its songs are listed
	// with the rest. Nothing is copied or changed on the drive.
	let {
		variant = 'ghost',
		onresult
	}: {
		variant?: 'ghost' | 'primary';
		/** Also told the outcome: the empty song list that holds this button
		 *  is replaced by the songs, so it reports it in its own status. */
		onresult?: (message: string) => void;
	} = $props();

	let browsing = $state(false);
	let adding = $state(false);
	/** The outcome, shown and read out. */
	let note = $state('');
	let error = $state<string | null>(null);

	async function add(path: string) {
		adding = true;
		error = null;
		note = `Adding ${path}… Vibecut is reading its songs.`;
		const before = s.tracks.length;
		try {
			const r = await s.addMusicFolder(path);
			const added = s.tracks.length - before;
			note = r.already
				? `${r.path} is already in your library.`
				: `Added ${r.path}: ${added === 1 ? '1 song' : `${added} songs`}.`;
			onresult?.(note);
		} catch (e) {
			note = '';
			error = e instanceof api.ApiError ? e.describe() : String(e);
			onresult?.(error);
		} finally {
			adding = false;
		}
	}
</script>

<div class="folder-pick">
	<button
		class="btn {variant}"
		type="button"
		aria-disabled={adding}
		onclick={() => {
			if (!adding) browsing = true;
		}}>{adding ? 'Adding the folder…' : 'Pick a music folder…'}</button
	>
	<p class="note" role="status">{note}</p>
	{#if error}<p class="error">{error}</p>{/if}
</div>

<BrowseDialog bind:open={browsing} mode="music" onpick={(c) => void add(c.path)} />

<style>
	.folder-pick {
		display: flex;
		align-items: center;
		flex-wrap: wrap;
		gap: 4px 12px;
	}

	.note {
		margin: 0;
		font-size: var(--fs-sm);
		color: var(--text-dim);
	}

	.note:empty {
		display: none;
	}

	.error {
		margin: 0;
		flex-basis: 100%;
		color: var(--bad);
		font-size: var(--fs-sm);
	}
</style>
