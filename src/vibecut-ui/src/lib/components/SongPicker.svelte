<script lang="ts">
	import { onDestroy } from 'svelte';
	import { appState as s } from '$lib/state.svelte';
	import { musicFileUrl, type Track } from '$lib/api';
	import { clock } from '$lib/vibes';
	import EnergyBars from './EnergyBars.svelte';
	import MusicImport from './MusicImport.svelte';
	import { collectionLabel, licenceBadge } from '$lib/music';

	let { compact = false }: { compact?: boolean } = $props();

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

	const shown = $derived(mood === 'all' ? s.tracks : s.tracks.filter((t) => t.mood === mood));
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
				// Mid-render: offer to stop it and switch songs instead.
				const title = s.tracks.find((t) => t.path === path)?.title ?? 'Vibecut’s pick';
				s.requestChange(`Song: ${title}`, () => choose(path));
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

	onDestroy(() => {
		audio?.pause();
		audio = undefined;
	});
</script>

<section class="act" class:compact aria-labelledby={compact ? undefined : 'song-h'} aria-label={compact ? 'Song' : undefined}>
	{#if !compact}
		<div class="act-head">
			<span class="num" aria-hidden="true">3</span>
			<h2 id="song-h">Pick a song</h2>
			<span class="hint">Every cut lands on its beat.</span>
		</div>
	{/if}

	{#if s.loadingMusic && s.tracks.length === 0}
		<div class="list">
			{#each [0, 1, 2] as i (i)}<div class="row skeleton"></div>{/each}
		</div>
	{:else if s.tracks.length === 0}
		<div class="empty glass">
			<strong>Your music library is empty.</strong>
			<span
				>Put MP3, WAV or M4A files in the <code>projects/music</code> folder — Vibecut works out their
				tempo, beats and energy automatically. Without a song, videos keep their own sound.</span
			>
			<button class="btn" type="button" onclick={() => s.loadMusic()}>Look again</button>
		</div>
		<MusicImport />
	{:else}
		<button
			type="button"
			class="auto"
			class:on={s.selectedTrackPath === null}
			aria-pressed={s.selectedTrackPath === null}
			onclick={() => choose(null)}
		>
			<span class="auto-icon" aria-hidden="true">✦</span>
			<span class="auto-text">
				<strong>Let Vibecut pick</strong>
				<span
					>{#if auto && s.effectiveVibe}Picked for you: <em>{auto.title}</em>{#if autoTrack}{` · ${Math.round(
								autoTrack.bpm
							)} BPM`}{/if}{:else}Matched to the vibe you choose{/if}</span
				>
				{#if auto && s.effectiveVibe}<span class="why">{auto.reason}</span>{/if}
			</span>
		</button>

		<div class="moods" role="radiogroup" aria-label="Filter by mood">
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

		<ul class="list" aria-label="Songs">
			{#each shown as t, i (t.path)}
				{@const isOn = s.selectedTrackPath === t.path}
				{@const isPlaying = playing === t.path}
				{@const lic = licenceBadge(t)}
				<li class="row" class:on={isOn} class:playing={isPlaying} style="--i: {i}">
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
					<button type="button" class="pick" aria-pressed={isOn} onclick={() => choose(t.path)}>
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
					<div class="wave">
						<EnergyBars energy={t.energy} progress={isPlaying ? progress : 0} active={isPlaying} />
					</div>
					<span class="radio" aria-hidden="true">{isOn ? '●' : ''}</span>
				</li>
			{/each}
		</ul>
		<MusicImport />
	{/if}
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
