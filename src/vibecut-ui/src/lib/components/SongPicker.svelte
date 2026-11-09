<script lang="ts">
	import { onDestroy, tick } from 'svelte';
	import { appState as s } from '$lib/state.svelte';
	import * as api from '$lib/api';
	import { musicFileUrl, type Track } from '$lib/api';
	import { clock } from '$lib/vibes';
	import { radiogroup } from '$lib/radiogroup';
	import EnergyBars from './EnergyBars.svelte';
	import MusicImport from './MusicImport.svelte';
	import BrowseDialog from './BrowseDialog.svelte';
	import PendingChangeOffer from './PendingChangeOffer.svelte';
	import { collectionLabel, licenceBadge } from '$lib/music';

	let { compact = false }: { compact?: boolean } = $props();
	const uid = $props.id();
	let list: HTMLUListElement | undefined = $state();

	const MOODS: { key: Track['mood'] | 'all'; label: string }[] = [
		{ key: 'all', label: 'All' },
		{ key: 'chill', label: 'Chill' },
		{ key: 'warm', label: 'Warm' },
		{ key: 'upbeat', label: 'Upbeat' },
		{ key: 'driving', label: 'Driving' }
	];

	let mood = $state<Track['mood'] | 'all'>('all');
	let playing = $state<string | null>(null);
	let progress = $state(0);
	let audio: HTMLAudioElement | undefined;

	// ---- music folders, like the footage step: the library's folders
	// (yours, the library's, those picked on a drive) filter the list.
	let folder = $state('all');
	let browsing = $state(false);
	let showManual = $state(false);
	let manual = $state('');
	let adding = $state(false);
	let addError = $state<string | null>(null);

	/** The music folder a track came from (the deepest that holds it). */
	function dirOf(t: Track): string | null {
		let best: string | null = null;
		for (const d of s.musicDirs) {
			const p = d.path.replace(/\/+$/, '');
			if (t.path.startsWith(p + '/') && (!best || p.length > best.length)) best = d.path;
		}
		return best;
	}

	function folderName(d: api.MusicDir): string {
		if (d.path === s.musicImportDir) return 'Your music';
		const shownPath = (d.display ?? d.path).replace(/[\\/]+$/, '');
		return shownPath.split(/[\\/]/).pop() || shownPath;
	}

	const folders = $derived(
		s.musicDirs
			.map((d) => ({ dir: d, name: folderName(d), n: s.tracks.filter((t) => dirOf(t) === d.path).length }))
			.filter((f) => f.n > 0 || f.dir.picked)
	);
	// One folder only: the row would just repeat "All songs".
	const showFolders = $derived(folders.length >= 2 || folders.some((f) => f.dir.picked));
	const current = $derived(folders.find((f) => f.dir.path === folder) ?? null);
	$effect(() => {
		if (folder !== 'all' && !current) folder = 'all';
	});

	const inFolder = $derived(folder === 'all' ? s.tracks : s.tracks.filter((t) => dirOf(t) === folder));
	const shown = $derived(mood === 'all' ? inFolder : inFolder.filter((t) => t.mood === mood));

	async function addFolder(path: string) {
		if (adding) return;
		adding = true;
		addError = null;
		lookNote = `Adding ${path}… Vibecut is reading its songs.`;
		const before = s.tracks.length;
		try {
			const r = await s.addMusicFolder(path);
			const n = s.tracks.length - before;
			lookNote = r.already
				? `${r.path} is already in your library.`
				: `Added ${r.path}: ${n === 1 ? '1 song' : `${n} songs`}.`;
			if (!r.already && r.container_path) folder = r.container_path;
			showManual = false;
			manual = '';
			if (s.tracks.length > 0) {
				await tick();
				list?.querySelector<HTMLElement>('[role="radio"][aria-checked="true"], [role="radio"]')?.focus();
			}
		} catch (e) {
			addError = e instanceof api.ApiError ? e.describe() : String(e);
			lookNote = addError;
		} finally {
			adding = false;
		}
	}

	async function removeFolder(d: api.MusicDir) {
		const name = d.display ?? d.path;
		try {
			await s.removeMusicFolder(d.path);
			folder = 'all';
			lookNote = `Removed ${name} from your library. Its songs are still on the drive.`;
		} catch (e) {
			lookNote = e instanceof api.ApiError ? e.describe() : String(e);
		}
	}
	// The server picks the song ("Picked for you"); this only shows it.
	const auto = $derived(s.autoPick);
	const autoTrack = $derived(auto ? (s.tracks.find((t) => t.path === auto.path) ?? null) : null);

	function setBeat(t: Track | null) {
		const beat = t?.beat_interval_secs ?? s.effectiveTrack?.beat_interval_secs;
		document.documentElement.style.setProperty('--beat', `${beat && beat > 0.2 ? beat : 0.5}s`);
	}

	function toggle(t: Track) {
		if (!audio) audio = new Audio();
		if (playing === t.path) {
			audio.pause();
			playing = null;
			setBeat(null);
			return;
		}
		audio.src = musicFileUrl(t.path);
		// Start where the music gets going, not on a quiet intro.
		audio.currentTime = Math.max(0, t.loud_region[0]);
		audio.ontimeupdate = () => {
			if (audio && t.duration_secs > 0) progress = audio.currentTime / t.duration_secs;
		};
		audio.onended = () => {
			playing = null;
			setBeat(null);
		};
		void audio.play();
		playing = t.path;
		progress = 0;
		setBeat(t);
	}

	// In the Studio a pick changes the project and re-renders, and "Let
	// Vibecut pick" resolves to the actual auto pick so the project and
	// the label agree. Before the first video it's only a selection.
	const inStudio = $derived(s.proposeResult !== null && s.renderHistory.length > 0);

	function choose(path: string | null) {
		if (inStudio) {
			if (s.rendering) {
				// Mid-render: offer to stop it and switch songs instead,
				// right here above the list (the pick doesn't vanish).
				const title = s.tracks.find((t) => t.path === path)?.title ?? 'Vibecut’s pick';
				s.requestChange(`Song: ${title}`, () => choose(path), 'song');
				return;
			}
			const target = path ?? s.autoPick?.path ?? null;
			if (target && target !== s.effectiveTrack?.path) {
				void s.changeSong(target).then(() => {
					if (path === null && !s.configError) s.selectedTrackPath = null;
					s.persist();
				});
			} else {
				s.selectedTrackPath = path;
				s.persist();
			}
		} else {
			s.selectedTrackPath = path;
			s.persist();
		}
		setBeat(null);
	}

	// "Look again" on the empty library: the list may replace the button
	// it was pressed on, so the focus moves to the song that is picked and
	// the outcome is read from this component's own status line.
	let lookNote = $state('');
	/** The "Add music" panel, opened from the empty state too. */
	let importOpen = $state(false);
	let root: HTMLElement | undefined = $state();
	async function showSources() {
		importOpen = true;
		await tick();
		const first = root?.querySelector<HTMLElement>('.find a');
		first?.scrollIntoView({ block: 'nearest', behavior: 'smooth' });
		first?.focus();
	}
	async function lookAgain() {
		if (s.loadingMusic) return;
		lookNote = 'Looking for songs…';
		await s.loadMusic();
		const n = s.tracks.length;
		lookNote = `${n === 0 ? 'No songs' : n === 1 ? '1 song' : `${n} songs`} in your library.`;
		if (n > 0) {
			await tick();
			list?.querySelector<HTMLElement>('[role="radio"][aria-checked="true"]')?.focus();
		}
	}

	onDestroy(() => {
		audio?.pause();
		audio = undefined;
	});
</script>

<section bind:this={root} class="act" class:compact aria-labelledby={compact ? undefined : 'song-h'} aria-label={compact ? 'Song' : undefined}>
	{#if !compact}
		<div class="act-head">
			<span class="num" aria-hidden="true">3</span>
			<h2 id="song-h">Pick a song</h2>
			<span class="hint">Every cut lands on its beat.</span>
			<span class="head-actions">
				<button class="btn small browse" type="button" aria-haspopup="dialog" aria-disabled={adding} onclick={() => {
						if (!adding) browsing = true;
					}}>{adding ? 'Adding…' : 'Browse your drives…'}</button
				>
				<button class="btn ghost small" type="button" aria-expanded={showManual} onclick={() => (showManual = !showManual)}>
					{showManual ? 'Hide' : 'Type a folder'}
				</button>
				<button class="btn ghost small" type="button" aria-haspopup="dialog" onclick={() => (s.musicSheet = 'more')}>
					Free music…
				</button>
			</span>
		</div>
	{/if}
	<BrowseDialog bind:open={browsing} mode="music" onpick={(c) => void addFolder(c.path)} />

	{#if showManual}
		<form
			class="manual"
			onsubmit={(e) => {
				e.preventDefault();
				if (manual.trim()) void addFolder(manual.trim());
			}}
		>
			<label class="sr-only" for="{uid}-manual">A folder of songs on a drive, such as D:\Music</label>
			<input id="{uid}-manual" placeholder="A folder of songs, e.g. D:\Music\Road trip" bind:value={manual} />
			<button class="btn" type="submit" disabled={!manual.trim() || adding}>Add</button>
		</form>
	{/if}
	{#if addError}<p class="add-error">{addError}</p>{/if}

	<!-- Placeholders for the first read only: a later "Look again" keeps
	     what is on screen (and the focus on it) while it looks. -->
	{#if s.loadingMusic && !s.musicLoaded && s.tracks.length === 0}
		<div class="list">
			{#each [0, 1, 2] as i (i)}<div class="row skeleton"></div>{/each}
		</div>
	{:else if s.tracks.length === 0}
		<div class="empty glass">
			<strong>Your music library is empty.</strong>
			<span
				>Pick a folder of songs on any drive, or download free ones and save them in
				<code>{s.musicImportDirDisplay ?? 'projects/music'}</code> — Vibecut works out their tempo, beats and
				energy automatically. Without a song, videos keep their own sound.</span
			>
			<div class="empty-actions">
				<button class="btn primary" type="button" aria-haspopup="dialog" aria-disabled={adding} onclick={() => {
						if (!adding) browsing = true;
					}}>{adding ? 'Adding the folder…' : 'Pick a music folder…'}</button
				>
				<button class="btn ghost" type="button" onclick={showSources}>Where to get free music</button>
				<!-- aria-disabled keeps the focus on the button while it looks. -->
				<button class="btn ghost" type="button" aria-disabled={s.loadingMusic} onclick={lookAgain}
					>{s.loadingMusic ? 'Looking…' : 'Look again'}</button
				>
			</div>
		</div>
	{:else}
		{#if compact}
			<div class="compact-actions">
				<button class="btn small" type="button" aria-haspopup="dialog" onclick={() => (s.musicSheet = 'songs')}
					>Hear songs over the video…</button
				>
				<button class="btn ghost small" type="button" aria-haspopup="dialog" onclick={() => (s.musicSheet = 'more')}
					>Get more music</button
				>
			</div>
		{/if}
		{#if showFolders}
			<div class="folders-row">
				<div class="folders" role="radiogroup" aria-label="Music folder" use:radiogroup>
					<button
						type="button"
						role="radio"
						aria-checked={folder === 'all'}
						class="folder-chip"
						class:on={folder === 'all'}
						onclick={() => (folder = 'all')}
						><span class="f-icon" aria-hidden="true">♫</span> All songs <span class="n">{s.tracks.length}</span></button
					>
					{#each folders as f (f.dir.path)}
						<button
							type="button"
							role="radio"
							aria-checked={folder === f.dir.path}
							class="folder-chip"
							class:on={folder === f.dir.path}
							title={f.dir.display ?? f.dir.path}
							onclick={() => (folder = f.dir.path)}
							><span class="f-icon" aria-hidden="true">{f.dir.picked ? '⛁' : '▣'}</span>
							{f.name} <span class="n">{f.n}</span></button
						>
					{/each}
				</div>
				{#if current?.dir.picked}
					<button class="btn ghost small" type="button" onclick={() => current && removeFolder(current.dir)}
						>Remove this folder</button
					>
				{/if}
			</div>
			{#if current}<p class="folder-path mono" title={current.dir.display ?? current.dir.path}>
					{current.dir.display ?? current.dir.path}
				</p>{/if}
		{/if}

		<div class="moods" role="radiogroup" aria-label="Filter by mood" use:radiogroup>
			{#each MOODS as m (m.key)}
				<button
					type="button"
					role="radio"
					aria-checked={mood === m.key}
					class="mood-chip {m.key}"
					class:on={mood === m.key}
					onclick={() => (mood = m.key)}>{m.label}</button
				>
			{/each}
		</div>

		{#if inStudio}<PendingChangeOffer
				from="song"
				returnFocus={() => list?.querySelector<HTMLElement>('[role="radio"][aria-checked="true"]') ?? null}
			/>{/if}

		<!-- In the Studio a pick re-renders, so arrows only move focus there
		     and Space / Enter picks. "Let Vibecut pick" is the first choice
		     of the same group, so exactly one choice is always checked. -->
		<ul class="list" role="radiogroup" aria-label="Song" use:radiogroup={{ selectOnMove: !inStudio }} bind:this={list}>
			<li class="auto-row" role="none">
				<button
					type="button"
					class="auto"
					class:on={s.selectedTrackPath === null}
					role="radio"
					aria-checked={s.selectedTrackPath === null}
					aria-labelledby="{uid}-auto"
					aria-describedby="{uid}-auto-why {uid}-auto-reason"
					onclick={() => choose(null)}
				>
					<span class="auto-icon" aria-hidden="true">✦</span>
					<span class="auto-text">
						<strong id="{uid}-auto">Let Vibecut pick</strong>
						<span id="{uid}-auto-why"
							>{#if auto && s.effectiveVibe}Picked for you: <em>{auto.title}</em>{#if autoTrack}{` · ${Math.round(
										autoTrack.bpm
									)} BPM`}{/if}{:else}Matched to the vibe you choose{/if}</span
						>
						{#if auto && s.effectiveVibe}<span class="why" id="{uid}-auto-reason">{auto.reason}</span>{/if}
					</span>
				</button>
			</li>
			{#each shown as t, i (t.path)}
				{@const isOn = s.selectedTrackPath === t.path}
				{@const isPlaying = playing === t.path}
				{@const lic = licenceBadge(t)}
				<li class="row" role="none" class:on={isOn} class:playing={isPlaying} style="--i: {i}">
					<button
						type="button"
						class="play"
						aria-label={isPlaying ? `Stop ${t.title}` : `Play ${t.title}`}
						onclick={() => toggle(t)}
					>
						{#if isPlaying}
							<svg viewBox="0 0 24 24" width="16" height="16" aria-hidden="true"
								><rect x="6" y="5" width="4" height="14" rx="1" fill="currentColor" /><rect
									x="14"
									y="5"
									width="4"
									height="14"
									rx="1"
									fill="currentColor"
								/></svg
							>
						{:else}
							<svg viewBox="0 0 24 24" width="16" height="16" aria-hidden="true"
								><path d="M8 5.5v13l11-6.5z" fill="currentColor" /></svg
							>
						{/if}
					</button>
					<!-- The licence badge is in the name; its note (hover title for
					     the mouse) is the description, for the keyboard too. -->
					<button
						type="button"
						class="pick"
						role="radio"
						aria-checked={isOn}
						aria-describedby="{uid}-lic-{i}"
						onclick={() => choose(t.path)}
					>
						<span class="title">{t.title}</span>
						<span class="facts">
							<span class="mood {t.mood}">{t.mood}</span>
							<span class="mono">{Math.round(t.bpm)} BPM</span>
							<span class="mono">{clock(t.duration_secs)}</span>
							<span class="lic {lic.tone}" title={t.licence.note}>{lic.text}</span>
							{#if t.collection === 'starter'}<span class="starter">{collectionLabel(t.collection)}</span>{/if}
							{#if auto?.path === t.path}<span class="picked">Picked for you</span>{/if}
							{#if t.grid_tight}<span class="tight" title="Clear, steady beat — cuts will lock precisely"
									>beat-lock ready</span
								>{/if}
						</span>
					</button>
					<span class="sr-only" id="{uid}-lic-{i}"
						>{t.licence.note}{t.attribution_required ? ' Posting it needs a credit in the caption.' : ''}</span
					>
					<div class="wave">
						<EnergyBars energy={t.energy} progress={isPlaying ? progress : 0} active={isPlaying} />
					</div>
					<span class="radio" aria-hidden="true">{isOn ? '●' : ''}</span>
				</li>
			{/each}
		</ul>
	{/if}
	<p class="sr-only" role="status">{lookNote}</p>
	<!-- One import view for every state of the list, so the panel the user
	     opened (and the focus in it) stays put while the library reloads. -->
	<MusicImport bind:open={importOpen} />
</section>

<style>
	.act {
		display: flex;
		flex-direction: column;
		gap: 14px;
	}

	.act-head {
		display: flex;
		align-items: center;
		gap: 12px;
		flex-wrap: wrap;
	}

	.num {
		display: inline-grid;
		place-items: center;
		width: 30px;
		height: 30px;
		border-radius: 50%;
		font-family: var(--serif);
		font-style: italic;
		font-size: 20px;
		background: var(--grad-brand-soft);
		border: 1px solid var(--line-strong);
	}

	h2 {
		margin: 0;
		font-size: var(--fs-xl);
		font-weight: 700;
		letter-spacing: -0.02em;
	}

	.hint {
		color: var(--text-faint);
		font-size: var(--fs-sm);
	}

	.auto-row {
		display: flex;
		flex-direction: column;
	}

	/* The list scrolls, which would clip an outside focus ring. */
	.auto:focus-visible {
		outline-offset: -2px;
	}

	.auto {
		display: flex;
		align-items: center;
		gap: 14px;
		padding: 14px 18px;
		border-radius: var(--r-md);
		border: 1px solid var(--line-strong);
		background: rgba(255, 255, 255, 0.03);
		text-align: left;
		cursor: pointer;
		transition: all var(--dur-2) var(--ease-out);
	}

	.auto.on {
		background: var(--grad-brand-soft);
		border-color: rgba(255, 61, 129, 0.55);
		box-shadow: 0 14px 40px -18px rgba(255, 61, 129, 0.7);
	}

	.auto-icon {
		display: grid;
		place-items: center;
		width: 38px;
		height: 38px;
		border-radius: 50%;
		background: var(--grad-brand);
		color: #160a12;
		font-size: 18px;
		animation: beatglow var(--beat) var(--ease-out) infinite;
	}

	.auto-text {
		display: flex;
		flex-direction: column;
	}

	.auto-text span {
		color: var(--text-dim);
		font-size: var(--fs-sm);
	}

	.auto-text em {
		font-family: var(--serif);
		font-size: 17px;
		color: var(--text);
	}

	.moods {
		display: flex;
		flex-wrap: wrap;
		gap: 6px;
	}

	.compact-actions {
		display: flex;
		flex-wrap: wrap;
		gap: 8px;
	}

	.head-actions {
		display: flex;
		gap: 8px;
		margin-left: auto;
	}

	.manual {
		display: flex;
		gap: 8px;
	}

	.manual input {
		flex: 1;
		min-width: 0;
		padding: 8px 12px;
		border-radius: var(--r-md);
		border: 1px solid var(--line-strong);
		background: rgba(0, 0, 0, 0.25);
		color: var(--text);
		font: inherit;
		font-size: var(--fs-sm);
	}

	.add-error {
		margin: 0;
		color: var(--bad);
		font-size: var(--fs-sm);
	}

	.folders-row {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 8px 12px;
	}

	.folders {
		display: flex;
		flex-wrap: wrap;
		gap: 8px;
	}

	.folder-chip {
		display: inline-flex;
		align-items: center;
		gap: 6px;
		max-width: 260px;
		padding: 7px 12px;
		border-radius: var(--r-md);
		border: 1px solid var(--line);
		background: rgba(255, 255, 255, 0.03);
		color: var(--text-dim);
		font-size: var(--fs-sm);
		font-weight: 600;
		cursor: pointer;
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}

	.folder-chip:hover {
		border-color: var(--line-strong);
		color: var(--text);
	}

	.folder-chip.on {
		color: var(--text);
		border-color: var(--magenta);
		background: color-mix(in srgb, var(--magenta) 14%, transparent);
	}

	.folder-chip .n {
		padding: 0 7px;
		border-radius: 999px;
		background: rgba(255, 255, 255, 0.08);
		font-size: var(--fs-xs);
		color: var(--text-dim);
	}

	.f-icon {
		opacity: 0.8;
	}

	.folder-path {
		margin: -6px 0 0;
		font-size: var(--fs-xs);
		color: var(--text-faint);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	@media (max-width: 640px) {
		.head-actions {
			margin-left: 0;
			width: 100%;
		}
	}

	.mood-chip {
		border: 1px solid var(--line);
		background: transparent;
		color: var(--text-dim);
		border-radius: 999px;
		padding: 5px 12px;
		font-size: var(--fs-xs);
		font-weight: 600;
		cursor: pointer;
	}

	.mood-chip.on {
		color: var(--ink);
		background: var(--text);
		border-color: var(--text);
	}

	.list {
		list-style: none;
		margin: 0;
		padding: 0;
		display: flex;
		flex-direction: column;
		gap: 8px;
		max-height: 380px;
		overflow-y: auto;
		scrollbar-width: thin;
	}

	.compact .list {
		max-height: 300px;
	}

	.row {
		display: grid;
		grid-template-columns: auto minmax(0, 1.3fr) minmax(90px, 1fr) 18px;
		align-items: center;
		gap: 14px;
		padding: 10px 14px 10px 10px;
		border-radius: var(--r-md);
		border: 1px solid var(--line);
		background: rgba(255, 255, 255, 0.025);
		animation: rise var(--dur-3) var(--ease-out) both;
		animation-delay: calc(var(--i, 0) * 40ms);
		transition:
			background var(--dur-1),
			border-color var(--dur-1);
	}

	.row:hover {
		background: rgba(255, 255, 255, 0.05);
	}

	.row.on {
		border-color: rgba(255, 61, 129, 0.6);
		background: rgba(255, 61, 129, 0.08);
	}

	.row.skeleton {
		height: 60px;
		background: linear-gradient(100deg, #151923 30%, #1f2533 50%, #151923 70%);
		background-size: 300% 100%;
		animation: shimmer 1.4s linear infinite;
	}

	.play {
		width: 40px;
		height: 40px;
		border-radius: 50%;
		border: none;
		display: grid;
		place-items: center;
		background: rgba(255, 255, 255, 0.08);
		cursor: pointer;
		transition: transform var(--dur-1) var(--ease-out);
	}

	.playing .play {
		background: var(--grad-brand);
		color: #160a12;
		animation: beatglow var(--beat) var(--ease-out) infinite;
	}

	.play:hover {
		transform: scale(1.06);
	}

	.pick {
		display: flex;
		flex-direction: column;
		align-items: flex-start;
		gap: 3px;
		background: none;
		border: none;
		padding: 0;
		text-align: left;
		cursor: pointer;
		min-width: 0;
	}

	.title {
		font-weight: 700;
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
		max-width: 100%;
	}

	.facts {
		display: flex;
		flex-wrap: wrap;
		gap: 8px;
		font-size: var(--fs-xs);
		color: var(--text-faint);
	}

	.mood {
		text-transform: capitalize;
		font-weight: 700;
	}
	.mood.chill {
		color: var(--cyan);
	}
	.mood.warm {
		color: #ffc94a;
	}
	.mood.upbeat {
		color: var(--lime);
	}
	.mood.driving {
		color: var(--magenta);
	}

	.tight {
		color: var(--good);
	}

	.auto-text .why {
		font-size: var(--fs-xs);
		color: var(--text-faint);
		margin-top: 2px;
	}

	.lic.ok {
		color: var(--good);
	}
	.lic.credit {
		color: var(--cyan);
	}
	.lic.caution {
		color: #ffc94a;
	}

	.starter {
		color: var(--lime);
		font-weight: 700;
	}

	.picked {
		color: var(--magenta);
		font-weight: 700;
	}

	.wave {
		min-width: 0;
	}

	.radio {
		width: 18px;
		height: 18px;
		border-radius: 50%;
		border: 1.5px solid var(--line-strong);
		display: grid;
		place-items: center;
		color: var(--magenta);
		font-size: 11px;
	}

	.on .radio {
		border-color: var(--magenta);
	}

	.empty {
		display: flex;
		flex-direction: column;
		align-items: flex-start;
		gap: 8px;
		padding: 18px 20px;
		color: var(--text-dim);
	}

	.empty-actions {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 8px;
	}

	.empty strong {
		color: var(--text);
	}

	@media (max-width: 640px) {
		.row {
			grid-template-columns: auto minmax(0, 1fr) 18px;
		}
		.wave {
			display: none;
		}
	}

	@keyframes rise {
		from {
			opacity: 0;
			transform: translateY(10px);
		}
	}

	@keyframes shimmer {
		from {
			background-position: 100% 0;
		}
		to {
			background-position: -100% 0;
		}
	}

	@keyframes beatglow {
		0% {
			box-shadow: 0 0 0 0 rgba(255, 61, 129, 0.7);
		}
		80%,
		100% {
			box-shadow: 0 0 0 12px rgba(255, 61, 129, 0);
		}
	}
</style>
