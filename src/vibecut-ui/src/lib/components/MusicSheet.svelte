<script lang="ts">
	import { tick, untrack } from 'svelte';
	import * as api from '$lib/api';
	import { appState as s } from '$lib/state.svelte';
	import { fitBadges, licenceBadge, MUSIC_SOURCES } from '$lib/music';
	import { clock } from '$lib/vibes';
	import BrowseDialog from './BrowseDialog.svelte';

	// "Change the music": choose a song by hearing it (alone, or over the
	// video you made), see how it fits, and get more songs without leaving.
	// Opened with `s.musicSheet = 'songs' | 'more'`.
	const uid = $props.id();
	let dialog: HTMLDialogElement | undefined = $state();
	let opener: HTMLElement | null = null;
	let listEl: HTMLElement | undefined = $state();

	let selected = $state<string | null>(null);
	let query = $state('');
	/** What's playing: a song alone, or a song over the video. */
	let playing = $state<'song' | 'over' | null>(null);
	let audio: HTMLAudioElement | undefined;
	let video: HTMLVideoElement | undefined = $state();
	let note = $state('');

	let suggestions = $state<api.SuggestedFolder[]>([]);
	let browsing = $state(false);
	let manual = $state('');
	let adding = $state(false);
	let addError = $state<string | null>(null);

	const tab = $derived(s.musicSheet ?? 'songs');
	const current = $derived(s.effectiveTrack?.path ?? null);
	const madeVideo = $derived(!!s.proposeResult && s.renderHistory.length > 0 && !!s.outputPath);
	const fresh = $derived(s.newTrackPaths);
	const vibe = $derived.by(() => {
		const v = s.vibes.find((x) => x.key === s.effectiveVibe);
		return v?.music ? { name: v.name, bpm: v.music.bpm } : null;
	});
	const targetSecs = $derived(madeVideo ? (s.videoDuration ?? 0) : 0);

	// New songs first, then the one in the video, then the rest.
	const shown = $derived.by(() => {
		const q = query.trim().toLowerCase();
		const list = q
			? s.tracks.filter((t) => `${t.title} ${t.artist ?? ''} ${t.mood}`.toLowerCase().includes(q))
			: s.tracks;
		const rank = (p: string) => (fresh.has(p) ? 0 : p === current ? 1 : 2);
		return [...list].sort((a, b) => rank(a.path) - rank(b.path));
	});
	const pick = $derived(s.tracks.find((t) => t.path === selected) ?? null);

	$effect(() => {
		if (!dialog) return;
		const want = s.musicSheet !== null;
		if (want && !dialog.open) {
			opener = document.activeElement as HTMLElement | null;
			dialog.showModal();
			untrack(() => void start());
		} else if (!want && dialog.open) dialog.close();
	});

	async function start() {
		selected = current ?? s.tracks[0]?.path ?? null;
		query = '';
		note = '';
		addError = null;
		if (!s.musicLoaded && !s.loadingMusic) await s.loadMusic();
		if (!selected) selected = s.tracks[0]?.path ?? null;
		void loadSuggestions();
		await tick();
		focusSelected();
	}

	async function loadSuggestions() {
		try {
			suggestions = (await api.musicSuggestions()).folders;
		} catch {
			suggestions = [];
		}
	}

	function focusSelected() {
		listEl?.querySelector<HTMLElement>('[role="radio"][aria-checked="true"]')?.focus();
	}

	function close() {
		s.musicSheet = null;
	}

	function onClose() {
		stop();
		s.musicSheet = null;
		s.markTracksSeen();
		if (opener?.isConnected) opener.focus();
		opener = null;
	}

	// ---- listening ----
	function stop() {
		audio?.pause();
		video?.pause();
		playing = null;
	}

	function startAudio(t: api.Track) {
		if (!audio) audio = new Audio();
		audio.src = api.musicFileUrl(t.path);
		audio.currentTime = Math.max(0, t.loud_region[0]);
		audio.onended = () => stop();
		return audio.play();
	}

	async function playSong(t: api.Track) {
		if (playing === 'song' && selected === t.path) return stop();
		stop();
		selected = t.path;
		playing = 'song';
		try {
			await startAudio(t);
		} catch {
			playing = null;
		}
	}

	/** The video you made, muted, with this song from where it gets going. */
	async function hearOver() {
		if (!pick || !video) return;
		if (playing === 'over') return stop();
		stop();
		playing = 'over';
		video.muted = true;
		video.currentTime = 0;
		try {
			await Promise.all([video.play(), startAudio(pick)]);
		} catch {
			stop();
		}
	}

	function use() {
		if (!pick || pick.path === current) return;
		stop();
		s.useSong(pick.path);
		close();
	}

	// ---- getting more ----
	let lastScan = 0;
	async function rescan(reason: string) {
		if (s.loadingMusic || Date.now() - lastScan < 3000) return;
		lastScan = Date.now();
		const before = s.newTrackPaths.size;
		await s.loadMusic();
		void loadSuggestions();
		const added = s.newTrackPaths.size - before;
		if (added > 0) note = `${added === 1 ? '1 new song' : `${added} new songs`} in your library.`;
		else if (reason === 'button') note = `No new songs. ${s.tracks.length} in your library.`;
	}

	// Back from a download tab: look for what was saved.
	function onFocusBack() {
		if (s.musicSheet !== null && document.visibilityState === 'visible') void rescan('focus');
	}

	async function addFolder(path: string) {
		if (adding) return;
		adding = true;
		addError = null;
		note = `Reading ${path}…`;
		const before = s.tracks.length;
		try {
			const r = await s.addMusicFolder(path);
			const n = s.tracks.length - before;
			note = r.already ? `${r.path} is already in your library.` : `Added ${r.path}: ${n === 1 ? '1 song' : `${n} songs`}.`;
			manual = '';
			void loadSuggestions();
			if (n > 0) {
				s.musicSheet = 'songs';
				const firstNew = s.tracks.find((t) => s.newTrackPaths.has(t.path));
				if (firstNew) selected = firstNew.path;
				await tick();
				focusSelected();
			}
		} catch (e) {
			addError = e instanceof api.ApiError ? e.describe() : String(e);
			note = addError;
		} finally {
			adding = false;
		}
	}

	function onListKey(e: KeyboardEvent) {
		const i = shown.findIndex((t) => t.path === selected);
		let j = i;
		if (e.key === 'ArrowDown' || e.key === 'ArrowRight') j = Math.min(shown.length - 1, i + 1);
		else if (e.key === 'ArrowUp' || e.key === 'ArrowLeft') j = Math.max(0, i - 1);
		else if (e.key === 'Home') j = 0;
		else if (e.key === 'End') j = shown.length - 1;
		else if (e.key === 'Enter') {
			e.preventDefault();
			use();
			return;
		} else return;
		e.preventDefault();
		if (j >= 0 && shown[j]) {
			if (playing) stop();
			selected = shown[j].path;
			void tick().then(focusSelected);
		}
	}
</script>

<svelte:window onfocus={onFocusBack} />
<svelte:document onvisibilitychange={onFocusBack} />

<dialog bind:this={dialog} class="sheet" aria-labelledby="{uid}-h" onclose={onClose}>
	<header class="head">
		<div>
			<p class="eyebrow">{current ? `Now: ♪ ${s.effectiveTrack?.title}` : 'No song chosen yet'}</p>
			<h2 id="{uid}-h">Change the music</h2>
		</div>
		<button class="close" type="button" aria-label="Close" onclick={close}>✕</button>
	</header>

	<div class="tabs" role="tablist" aria-label="Music">
		<button
			type="button"
			role="tab"
			id="{uid}-tab-songs"
			aria-selected={tab === 'songs'}
			aria-controls="{uid}-panel"
			class:on={tab === 'songs'}
			onclick={() => (s.musicSheet = 'songs')}>Your songs <span class="n">{s.tracks.length}</span></button
		>
		<button
			type="button"
			role="tab"
			id="{uid}-tab-more"
			aria-selected={tab === 'more'}
			aria-controls="{uid}-panel"
			class:on={tab === 'more'}
			onclick={() => {
				stop();
				s.musicSheet = 'more';
			}}>Get more music</button
		>
	</div>

	<div class="body" role="tabpanel" id="{uid}-panel" aria-labelledby="{uid}-tab-{tab}">
		{#if tab === 'songs'}
			{#if s.tracks.length === 0}
				<div class="empty">
					<strong>No songs yet.</strong>
					<span>Get some free ones, or read a folder of songs you already have.</span>
					<button class="btn primary" type="button" onclick={() => (s.musicSheet = 'more')}>Get more music</button>
				</div>
			{:else}
				<div class="songs">
					<div class="list-col">
						<label class="sr-only" for="{uid}-q">Find a song</label>
						<input id="{uid}-q" class="search" type="search" placeholder="Find a song, mood or artist" bind:value={query} />
						<ul class="list" role="radiogroup" aria-label="Songs" bind:this={listEl} onkeydown={onListKey}>
							{#each shown as t (t.path)}
								{@const on = selected === t.path}
								<li role="none">
									<!-- One tab stop: arrows move, Enter uses the song. -->
									<button
										type="button"
										role="radio"
										aria-checked={on}
										tabindex={on ? 0 : -1}
										class="row"
										class:on
										onclick={() => {
											if (selected !== t.path && playing) stop();
											selected = t.path;
										}}
									>
										<span class="t">
											{t.title}
											{#if t.path === current}<span class="tag now">In the video</span>{/if}
											{#if fresh.has(t.path)}<span class="tag new">New</span>{/if}
										</span>
										<span class="m">{t.mood} · {Math.round(t.bpm)} BPM · {clock(t.duration_secs)}</span>
									</button>
								</li>
							{:else}
								<li class="none" role="presentation">No song matches “{query}”.</li>
							{/each}
						</ul>
					</div>

					{#if pick}
						{@const badges = fitBadges(pick, vibe, targetSecs)}
						{@const lic = licenceBadge(pick)}
						<div class="detail">
							<div>
								<p class="eyebrow">{pick.artist ?? 'Song'}</p>
								<h3>{pick.title}</h3>
								<p class="meta">{pick.mood} · {Math.round(pick.bpm)} BPM · {clock(pick.duration_secs)} · <span class="lic {lic.tone}">{lic.text}</span></p>
							</div>
							{#if badges.length}
								<ul class="fit" aria-label="How it fits">
									{#each badges as b (b.text)}<li class={b.tone}>{b.text}</li>{/each}
								</ul>
							{/if}
							{#if madeVideo && s.outputPath}
								<!-- svelte-ignore a11y_media_has_caption -->
								<video
									class="preview"
									src="{api.outputFileUrl(s.outputPath)}#t=1"
									bind:this={video}
									muted
									playsinline
									preload="metadata"
									onended={stop}
								></video>
							{/if}
							<div class="actions">
								<button class="btn ghost" type="button" onclick={() => playSong(pick)}>
									{playing === 'song' ? '❚❚ Stop' : '▶ Play the song'}
								</button>
								{#if madeVideo}
									<button class="btn ghost" type="button" onclick={hearOver}>
										{playing === 'over' ? '❚❚ Stop' : '▶ Hear it over my video'}
									</button>
								{/if}
							</div>
							{#if playing === 'over'}
								<p class="hint">A rough listen: the render will cut your clips to this song's beat.</p>
							{/if}
							<button class="btn primary use" type="button" disabled={pick.path === current} onclick={use}>
								{pick.path === current ? 'Already in this video' : madeVideo ? `Use ${pick.title}` : `Choose ${pick.title}`}
							</button>
						</div>
					{/if}
				</div>
			{/if}
		{:else}
			<div class="more">
				<section>
					<h3>Already have songs?</h3>
					{#if suggestions.length}
						<p class="dim">These folders have songs Vibecut isn't reading yet:</p>
						<ul class="suggest">
							{#each suggestions as f (f.container_path)}
								<li>
									<button class="btn" type="button" aria-disabled={adding} onclick={() => void addFolder(f.path)}>
										Read <span class="mono">{f.path}</span>
									</button>
									<span class="dim">{f.n_songs}{f.n_songs >= 500 ? '+' : ''} song{f.n_songs === 1 ? '' : 's'}</span>
								</li>
							{/each}
						</ul>
					{/if}
					<div class="row-actions">
						<button class="btn" type="button" aria-haspopup="dialog" aria-disabled={adding} onclick={() => {
								if (!adding) browsing = true;
							}}>Browse your drives…</button
						>
						<form
							class="manual"
							onsubmit={(e) => {
								e.preventDefault();
								if (manual.trim()) void addFolder(manual.trim());
							}}
						>
							<label class="sr-only" for="{uid}-folder">A folder of songs on a drive, such as D:\Music</label>
							<input id="{uid}-folder" placeholder="Or type a folder, e.g. D:\Music" bind:value={manual} />
							<button class="btn ghost" type="submit" disabled={!manual.trim() || adding}>Add</button>
						</form>
					</div>
					{#if addError}<p class="error">{addError}</p>{/if}
				</section>

				<section>
					<h3>Download free songs</h3>
					<p class="dim">
						These sites let you use their songs in your videos (links open in a new tab). Save them in
						<code>{s.musicImportDirDisplay ?? 'projects/music'}</code> or a folder Vibecut reads. When you come back
						here, Vibecut looks for new songs.
					</p>
					<ul class="sources">
						{#each MUSIC_SOURCES as src (src.url)}
							<li>
								<a href={src.url} target="_blank" rel="noopener noreferrer"
									>{src.name}<span class="sr-only"> (opens in a new tab)</span> <span aria-hidden="true">↗</span></a
								>
								<span class="badge {src.tone}">{src.badge}</span>
								<span class="dim small">{src.terms}</span>
							</li>
						{/each}
					</ul>
					<button class="btn ghost" type="button" aria-disabled={s.loadingMusic} onclick={() => void rescan('button')}
						>{s.loadingMusic ? 'Looking…' : 'Look for new songs now'}</button
					>
				</section>
			</div>
		{/if}
	</div>
	<p class="status" role="status">{note}</p>
</dialog>

<BrowseDialog bind:open={browsing} mode="music" onpick={(c) => void addFolder(c.path)} />

<style>
	.sheet {
		width: min(880px, calc(100vw - 24px));
		max-height: min(820px, calc(100vh - 32px));
		padding: 0;
		border-radius: var(--r-xl);
		border: 1px solid var(--line-strong);
		background: rgba(14, 16, 23, 0.98);
		color: var(--text);
		box-shadow: var(--shadow-2);
		overflow: hidden;
		flex-direction: column;
	}

	.sheet[open] {
		display: flex;
	}

	.sheet::backdrop {
		background: rgba(3, 4, 7, 0.65);
		backdrop-filter: blur(8px);
	}

	.head {
		display: flex;
		justify-content: space-between;
		align-items: flex-start;
		gap: 12px;
		padding: 20px 22px 8px;
	}

	.head h2 {
		margin: 2px 0 0;
		font-size: var(--fs-xl);
		letter-spacing: -0.02em;
	}

	.close {
		border: 1px solid var(--line);
		background: transparent;
		color: var(--text-dim);
		border-radius: 999px;
		width: 34px;
		height: 34px;
		cursor: pointer;
	}

	.tabs {
		display: flex;
		gap: 6px;
		padding: 6px 22px 12px;
		border-bottom: 1px solid var(--line);
	}

	.tabs button {
		border: 1px solid var(--line);
		background: transparent;
		color: var(--text-dim);
		border-radius: 999px;
		padding: 6px 14px;
		font-weight: 600;
		font-size: var(--fs-sm);
		cursor: pointer;
	}

	.tabs button.on {
		background: var(--text);
		color: var(--ink);
		border-color: var(--text);
	}

	.tabs .n {
		opacity: 0.7;
		margin-left: 4px;
	}

	.body {
		flex: 1;
		min-height: 0;
		overflow-y: auto;
		padding: 16px 22px;
	}

	.songs {
		display: grid;
		grid-template-columns: minmax(0, 1fr) minmax(0, 1fr);
		gap: 18px;
		min-height: 0;
	}

	.list-col {
		display: flex;
		flex-direction: column;
		gap: 8px;
		min-width: 0;
	}

	.search,
	.manual input {
		width: 100%;
		padding: 8px 12px;
		border-radius: var(--r-md);
		border: 1px solid var(--line-strong);
		background: rgba(0, 0, 0, 0.25);
		color: var(--text);
		font: inherit;
		font-size: var(--fs-sm);
	}

	.list {
		list-style: none;
		margin: 0;
		padding: 0;
		display: flex;
		flex-direction: column;
		gap: 4px;
		max-height: 440px;
		overflow-y: auto;
	}

	.row {
		width: 100%;
		display: flex;
		flex-direction: column;
		gap: 2px;
		text-align: left;
		padding: 9px 12px;
		border-radius: var(--r-md);
		border: 1px solid transparent;
		background: rgba(255, 255, 255, 0.02);
		color: var(--text);
		cursor: pointer;
	}

	.row:focus-visible {
		outline-offset: -2px;
	}

	.row:hover {
		border-color: var(--line);
	}

	.row.on {
		border-color: var(--magenta);
		background: color-mix(in srgb, var(--magenta) 12%, transparent);
	}

	.row .t {
		font-weight: 600;
		display: flex;
		align-items: center;
		gap: 6px;
		flex-wrap: wrap;
	}

	.row .m {
		font-size: var(--fs-xs);
		color: var(--text-dim);
		text-transform: capitalize;
	}

	.tag {
		font-size: 11px;
		font-weight: 700;
		padding: 0 7px;
		border-radius: 999px;
		border: 1px solid currentColor;
	}

	.tag.now {
		color: var(--cyan);
	}

	.tag.new {
		color: var(--magenta);
	}

	.none {
		color: var(--text-dim);
		font-size: var(--fs-sm);
		padding: 8px 4px;
	}

	.detail {
		display: flex;
		flex-direction: column;
		gap: 12px;
		min-width: 0;
	}

	.detail h3 {
		margin: 2px 0 4px;
		font-size: 20px;
	}

	.meta {
		margin: 0;
		color: var(--text-dim);
		font-size: var(--fs-sm);
		text-transform: capitalize;
	}

	.lic {
		text-transform: none;
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

	.fit {
		list-style: none;
		margin: 0;
		padding: 0;
		display: flex;
		flex-wrap: wrap;
		gap: 6px;
	}

	.fit li,
	.badge {
		font-size: var(--fs-xs);
		font-weight: 700;
		padding: 2px 9px;
		border-radius: 999px;
		border: 1px solid currentColor;
		white-space: nowrap;
	}

	.ok {
		color: var(--good);
	}
	.credit {
		color: var(--cyan);
	}
	.caution {
		color: #ffc94a;
	}
	.unknown {
		color: var(--text-faint);
	}

	.preview {
		width: 100%;
		aspect-ratio: 16 / 9;
		border-radius: var(--r-md);
		background: #000;
		border: 1px solid var(--line);
	}

	.actions {
		display: flex;
		flex-wrap: wrap;
		gap: 8px;
	}

	.use {
		align-self: flex-start;
	}

	.hint,
	.dim {
		margin: 0;
		color: var(--text-dim);
		font-size: var(--fs-sm);
	}

	.small {
		font-size: var(--fs-xs);
	}

	.empty {
		display: flex;
		flex-direction: column;
		align-items: flex-start;
		gap: 10px;
		padding: 18px;
	}

	.more {
		display: flex;
		flex-direction: column;
		gap: 22px;
	}

	.more section {
		display: flex;
		flex-direction: column;
		gap: 10px;
	}

	.more h3 {
		margin: 0;
		font-size: var(--fs-md, 16px);
	}

	.suggest {
		list-style: none;
		margin: 0;
		padding: 0;
		display: flex;
		flex-direction: column;
		gap: 6px;
	}

	.suggest li {
		display: flex;
		align-items: center;
		gap: 10px;
		flex-wrap: wrap;
	}

	.suggest .mono {
		font-size: var(--fs-xs);
	}

	.row-actions {
		display: flex;
		flex-wrap: wrap;
		gap: 10px;
		align-items: center;
	}

	.manual {
		display: flex;
		gap: 8px;
		flex: 1;
		min-width: 240px;
	}

	.sources {
		list-style: none;
		margin: 0;
		padding: 0;
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

	.sources .small {
		grid-column: 1 / -1;
	}

	code {
		word-break: break-all;
	}

	.error {
		margin: 0;
		color: var(--bad);
		font-size: var(--fs-sm);
	}

	.status {
		margin: 0;
		padding: 10px 22px 16px;
		font-size: var(--fs-sm);
		color: var(--text-dim);
		border-top: 1px solid var(--line);
	}

	.status:empty {
		display: none;
	}

	@media (max-width: 720px) {
		.songs {
			grid-template-columns: 1fr;
		}
		.list {
			max-height: 260px;
		}
	}
</style>
