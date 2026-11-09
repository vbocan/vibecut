<script lang="ts">
	import { appState as s } from '$lib/state.svelte';
	import * as api from '$lib/api';
	import { licenceBadge, MUSIC_SOURCES } from '$lib/music';
	import MusicFolderButton from './MusicFolderButton.svelte';

	// "Add music": where to download songs, where they go (or which folder
	// on a drive to read), and what each track's licence means for posting.
	// Vibecut can't ship most royalty-free music (the sites forbid
	// redistribution), so the user brings their own.
	let { open = $bindable(false) }: { open?: boolean } = $props();
	// Unique per instance: the song picker can show this in more than one place.
	const uid = $props.id();
	/** The outcome of "Look again", for screen readers. */
	let lookNote = $state('');

	async function lookAgain() {
		if (s.loadingMusic) return;
		lookNote = 'Looking for songs…';
		await s.loadMusic();
		const n = s.tracks.length;
		lookNote = `${n === 0 ? 'No songs' : n === 1 ? '1 song' : `${n} songs`} in your library${
			s.musicWarnings.length > 0 ? `; ${s.musicWarnings.length} couldn’t be used` : ''
		}.`;
	}

	const importDir = $derived(s.musicImportDir ?? '/projects/music');
	const importShown = $derived(s.musicImportDirDisplay ?? importDir);
	const importExists = $derived(s.musicDirs.find((d) => d.path === importDir)?.exists ?? true);
	const otherDirs = $derived(s.musicDirs.filter((d) => d.path !== importDir));
	let removeNote = $state('');

	async function remove(d: api.MusicDir) {
		const name = d.display ?? d.path;
		removeNote = `Removing ${name}…`;
		try {
			await s.removeMusicFolder(d.path);
			removeNote = `Removed ${name} from your library. Its songs are still on the drive.`;
		} catch (e) {
			removeNote = e instanceof api.ApiError ? e.describe() : String(e);
		}
	}
	const needCredit = $derived(s.tracks.filter((t) => t.attribution_required).length);
	const unknown = $derived(s.tracks.filter((t) => t.licence.kind === 'unknown').length);

	const example = `{
  "title": "Night Drive",
  "artist": "Ada Lovelace",
  "licence": "CC-BY-4.0",
  "source_url": "https://example.org/night-drive",
  "credit": "Night Drive by Ada Lovelace (CC BY 4.0)"
}`;
</script>

<div class="import">
	<button
		type="button"
		class="toggle"
		aria-expanded={open}
		aria-controls="{uid}-panel"
		onclick={() => (open = !open)}
	>
		<span class="plus" aria-hidden="true">{open ? '−' : '+'}</span>
		<span>Add music <span class="dim">· free songs, your folders</span></span>
		{#if !open && (needCredit > 0 || unknown > 0)}
			<span class="summary">
				{#if needCredit > 0}{needCredit} need credit{/if}{#if needCredit > 0 && unknown > 0} · {/if}{#if unknown > 0}{unknown}
					with unknown licence{/if}
			</span>
		{/if}
	</button>

	{#if open}
		<div id="{uid}-panel" class="panel">
			<section class="find" aria-labelledby="{uid}-find-h">
				<h3 id="{uid}-find-h">Find free songs</h3>
				<p class="dim">These sites let you use their songs in your videos. Each link opens in a new tab.</p>
				<ul class="sources">
					{#each MUSIC_SOURCES as src (src.url)}
						<li>
							<a href={src.url} target="_blank" rel="noopener noreferrer"
								>{src.name}<span class="sr-only"> (opens in a new tab)</span><span class="ext" aria-hidden="true">↗</span></a
							>
							<span class="badge {src.tone}">{src.badge}</span>
							<span class="note">{src.terms}</span>
						</li>
					{/each}
				</ul>
				<p class="dim">Terms change: check the site's licence before you post. For songs that need a credit, the licence section below shows how to record it.</p>
			</section>

			<section>
				<h3>Where to put your songs</h3>
				<p>
					Save MP3, WAV, M4A, FLAC or OGG files in <code class="path">{importShown}</code>{#if !importExists}
						— it doesn't exist yet, so create it{/if}, then press <strong>Look again</strong>. Subfolders are fine;
					Vibecut works out each song's tempo, beats and energy the first time it sees it.
				</p>
				<p>Already have a folder of songs? Pick it on any drive and Vibecut reads it where it is:</p>
				<MusicFolderButton />
				{#if otherDirs.length > 0}
					<h4>Also reading</h4>
					<ul class="dirs">
						{#each otherDirs as d (d.path)}
							<li>
								<code class="path">{d.display ?? d.path}</code>
								{#if !d.exists}<span class="dim">(not there now)</span>{/if}
								{#if d.picked}
									<button class="btn ghost small" type="button" aria-label="Remove {d.display ?? d.path} from your library" onclick={() => remove(d)}
										>Remove</button
									>
								{/if}
							</li>
						{/each}
					</ul>
				{/if}
				<p class="sr-only" role="status">{removeNote}</p>
				<!-- aria-disabled keeps the focus on the button while it looks. -->
				<button class="btn ghost" type="button" onclick={lookAgain} aria-disabled={s.loadingMusic}
					>{s.loadingMusic ? 'Looking…' : 'Look again'}</button
				>
				<p class="sr-only" role="status">{lookNote}</p>
			</section>

			<section>
				<h3>Licences</h3>
				<p class="dim">
					Songs from royalty-free sites come with that site's terms. Tell Vibecut what they are with a
					<code>song.json</code> next to <code>song.mp3</code>, or one <code>library.json</code> per folder:
				</p>
				<pre class="mono">{example}</pre>
				{#if s.tracks.length > 0}
					<ul class="tracks" aria-label="Licence of each song">
						{#each s.tracks as t (t.path)}
							{@const b = licenceBadge(t)}
							<li>
								<span class="name">
									<strong>{t.title}</strong>{#if t.artist}<span class="dim">{` · ${t.artist}`}</span>{/if}
								</span>
								<span class="badges">
									<!-- The badge's colour repeats its words; the words carry it. -->
									<span class="badge {b.tone}"
										><span class="sr-only">Licence: </span>{b.text}{#if t.attribution_required}<span class="sr-only"
												>, credit required</span
											>{/if}</span
									>
									{#if t.collection === 'starter'}<span class="badge starter">Starter pack</span>{/if}
								</span>
								<span class="note">{t.licence.note}</span>
							</li>
						{/each}
					</ul>
				{/if}
			</section>

			{#if s.musicWarnings.length > 0}
				<section class="warn" aria-labelledby="{uid}-warn-h">
					<h3 id="{uid}-warn-h">Couldn't use some of it</h3>
					<ul>
						{#each s.musicWarnings as w, i (i)}<li class="mono">{w}</li>{/each}
					</ul>
				</section>
			{/if}
		</div>
	{/if}
</div>

<style>
	.import {
		display: flex;
		flex-direction: column;
		gap: 10px;
	}

	.toggle {
		display: flex;
		align-items: center;
		gap: 10px;
		flex-wrap: wrap;
		padding: 10px 14px;
		border-radius: var(--r-md);
		border: 1px dashed var(--line-strong);
		background: transparent;
		color: var(--text);
		font-weight: 600;
		text-align: left;
		cursor: pointer;
	}

	.toggle:hover {
		background: rgba(255, 255, 255, 0.04);
	}

	.plus {
		display: grid;
		place-items: center;
		width: 22px;
		height: 22px;
		border-radius: 50%;
		background: rgba(255, 255, 255, 0.08);
	}

	.summary {
		margin-left: auto;
		font-size: var(--fs-xs);
		color: #ffc94a;
		font-weight: 600;
	}

	.dim {
		color: var(--text-faint);
		font-weight: 400;
	}

	.panel {
		display: flex;
		flex-direction: column;
		gap: 16px;
		padding: 14px 16px;
		border-radius: var(--r-md);
		border: 1px solid var(--line);
		background: rgba(255, 255, 255, 0.025);
	}

	section {
		display: flex;
		flex-direction: column;
		align-items: flex-start;
		gap: 8px;
	}

	h3 {
		margin: 0;
		font-size: var(--fs-sm);
		font-weight: 700;
	}

	p {
		margin: 0;
		font-size: var(--fs-sm);
		color: var(--text-dim);
		line-height: 1.5;
	}

	code {
		word-break: break-all;
	}

	pre {
		margin: 0;
		padding: 10px 12px;
		max-width: 100%;
		overflow-x: auto;
		border-radius: var(--r-sm, 8px);
		background: rgba(0, 0, 0, 0.3);
		font-size: var(--fs-xs);
	}

	.tracks {
		list-style: none;
		margin: 0;
		padding: 0;
		width: 100%;
		display: flex;
		flex-direction: column;
		gap: 8px;
		max-height: 260px;
		overflow-y: auto;
	}

	.tracks li {
		display: grid;
		grid-template-columns: minmax(0, 1fr) auto;
		gap: 2px 10px;
		padding: 8px 10px;
		border-radius: var(--r-sm, 8px);
		background: rgba(255, 255, 255, 0.03);
	}

	.name {
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.badges {
		display: flex;
		gap: 6px;
		justify-content: flex-end;
	}

	.note {
		grid-column: 1 / -1;
		font-size: var(--fs-xs);
		color: var(--text-dim);
	}

	.badge {
		font-size: var(--fs-xs);
		font-weight: 700;
		padding: 2px 8px;
		border-radius: 999px;
		border: 1px solid currentColor;
		white-space: nowrap;
	}

	.badge.ok {
		color: var(--good);
	}
	.badge.credit {
		color: var(--cyan);
	}
	.badge.caution {
		color: #ffc94a;
	}
	.badge.unknown {
		color: var(--text-faint);
	}
	.badge.starter {
		color: var(--lime);
	}

	h4 {
		margin: 4px 0 0;
		font-size: var(--fs-xs);
		font-weight: 700;
		color: var(--text-dim);
	}

	.sources,
	.dirs {
		list-style: none;
		margin: 0;
		padding: 0;
		width: 100%;
		display: flex;
		flex-direction: column;
		gap: 6px;
	}

	.sources li {
		display: grid;
		grid-template-columns: minmax(0, 1fr) auto;
		gap: 2px 10px;
		align-items: center;
		padding: 8px 10px;
		border-radius: var(--r-sm, 8px);
		background: rgba(255, 255, 255, 0.03);
	}

	.sources a {
		font-weight: 700;
		color: var(--text);
		text-decoration: underline;
		text-decoration-color: var(--line-strong);
		text-underline-offset: 3px;
	}

	.sources a:hover,
	.sources a:focus-visible {
		text-decoration-color: currentColor;
	}

	.ext {
		margin-left: 4px;
		font-size: var(--fs-xs);
		color: var(--text-dim);
	}

	.sources .badge {
		justify-self: end;
	}

	.dirs li {
		display: flex;
		align-items: center;
		flex-wrap: wrap;
		gap: 6px 10px;
		font-size: var(--fs-sm);
	}

	.path {
		font-size: var(--fs-xs);
	}

	.btn.small {
		padding: 4px 10px;
		font-size: var(--fs-xs);
	}

	.warn ul {
		margin: 0;
		padding-left: 18px;
		font-size: var(--fs-xs);
		color: #ffc94a;
	}

	@media (max-width: 640px) {
		.tracks li,
		.sources li {
			grid-template-columns: minmax(0, 1fr);
		}
		.sources .badge {
			justify-self: start;
		}
		.badges {
			justify-content: flex-start;
		}
	}
</style>
