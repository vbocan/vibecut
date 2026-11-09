<script lang="ts">
	import { appState as s } from '$lib/state.svelte';
	import { outputFileUrl } from '$lib/api';
	import { clock } from '$lib/vibes';
	import Player from './Player.svelte';
	import BeatTimeline from './BeatTimeline.svelte';
	import MomentInspector from './MomentInspector.svelte';
	import TweakPanel from './TweakPanel.svelte';
	import VersionsStrip from './VersionsStrip.svelte';
	import SongPicker from './SongPicker.svelte';
	import CommandBar from './CommandBar.svelte';
	import CuttingRoom from './CuttingRoom.svelte';
	import BeatClock from './BeatClock.svelte';
	import ShareSheet from './ShareSheet.svelte';

	let { onOpenPro }: { onOpenPro?: () => void } = $props();
	let shareOpen = $state(false);
	let helpOpen = $state(false);
	// Single-key shortcuts can be switched off (WCAG 2.1.4); remembered
	// per browser.
	const KEYS_PREF = 'vibecut.shortcuts';
	let keysOn = $state(readKeysPref());
	function readKeysPref(): boolean {
		try {
			return localStorage.getItem(KEYS_PREF) !== 'off';
		} catch {
			return true;
		}
	}
	function setKeysOn(on: boolean) {
		keysOn = on;
		try {
			localStorage.setItem(KEYS_PREF, on ? 'on' : 'off');
		} catch {
			/* not remembered */
		}
	}

	const SHORTCUTS: [string, string][] = [
		['Space / K', 'Play or pause'],
		['J / L', 'Back / forward 5 s'],
		['← / →', 'Previous / next moment'],
		['[ / ]', 'Shorter / longer'],
		['E / C', 'More energy / calmer'],
		['N / S / M', 'New song / shuffle / another mix'],
		['Del', 'Drop the selected moment'],
		['Ctrl Z', 'Undo the last change'],
		['Ctrl K', 'Tell it'],
		['D', 'Pro settings'],
		['?', 'This list']
	];

	function nextSongPath(): string | null {
		if (s.tracks.length === 0) return null;
		const i = s.tracks.findIndex((t) => t.path === s.effectiveTrack?.path);
		return s.tracks[(i + 1) % s.tracks.length].path;
	}

	function moveMoment(delta: number) {
		const n = s.segmentLayout.length;
		if (n === 0) return;
		const cur = s.selectedSegmentIndex ?? s.segmentAt(currentTime)?.index ?? (delta > 0 ? -1 : n);
		const i = Math.max(0, Math.min(n - 1, cur + delta));
		s.selectedSegmentIndex = i;
		seek(s.segmentLayout[i].startSecs + 0.05);
	}

	function onKey(e: KeyboardEvent) {
		const t = e.target as HTMLElement | null;
		if (t?.closest('input, textarea, select, [contenteditable="true"]')) return;
		if (document.querySelector('dialog[open]')) return;
		const mod = e.ctrlKey || e.metaKey;
		if (mod && e.key.toLowerCase() === 'k') {
			e.preventDefault();
			document.getElementById('cmd-input')?.focus();
			return;
		}
		if (mod && e.key.toLowerCase() === 'z') {
			if (s.undo) {
				e.preventDefault();
				void s.undoLast();
			}
			return;
		}
		if (mod || e.altKey) return;
		const k0 = e.key;
		// Space and Enter belong to whatever control has focus, and arrows
		// to radio groups, tabs and sliders.
		const control = t?.closest(
			'button, a[href], summary, [role="button"], [role="radio"], [role="tab"], [role="slider"], [role="checkbox"]'
		);
		if (control && (k0 === ' ' || k0 === 'Enter')) return;
		if (k0.startsWith('Arrow') && t?.closest('[role="radiogroup"], [role="tablist"], [role="slider"]')) return;
		if (k0 === '?') {
			helpOpen = !helpOpen;
			e.preventDefault();
			return;
		}
		if (k0 === 'Escape' && helpOpen) {
			helpOpen = false;
			return;
		}
		if (!keysOn) return;
		const inTimeline = !!t?.closest('.timeline');
		const onVideo = t?.tagName === 'VIDEO';
		const busy = s.feedbackBusy || s.renderMode === 'queued' || s.renderMode === 'running';
		const k = e.key;
		let handled = true;
		if ((k === ' ' && !onVideo) || k === 'k') {
			if (video) void (video.paused ? video.play() : video.pause());
		} else if (k === 'j') seek(Math.max(0, currentTime - 5));
		else if (k === 'l') seek(currentTime + 5);
		else if ((k === 'ArrowLeft' || k === 'ArrowRight') && !inTimeline && !onVideo) moveMoment(k === 'ArrowLeft' ? -1 : 1);
		else if (k === 'd') onOpenPro?.();
		else if (busy) handled = false;
		else if (k === '[') void s.tweakAndRender({ kind: 'scale_duration', factor: 0.75 }, 'Shorter');
		else if (k === ']') void s.tweakAndRender({ kind: 'scale_duration', factor: 1.25 }, 'Longer');
		else if (k === 'e') void s.tweakAndRender({ kind: 'punchier' }, 'More energy');
		else if (k === 'c') void s.tweakAndRender({ kind: 'calmer' }, 'Calmer');
		else if (k === 's') void s.tweakAndRender({ kind: 'reroll' }, 'Shuffle');
		else if (k === 'm') void s.anotherMix();
		else if (k === 'n') {
			const p = nextSongPath();
			if (p) void s.changeSong(p);
		} else if ((k === 'Delete' || k === 'Backspace') && s.selectedSegmentIndex !== null) {
			const g = s.segmentLayout[s.selectedSegmentIndex];
			if (g) void s.withUndo(`Dropped moment ${g.index + 1}`, () => s.dropSegment(g));
		} else handled = false;
		if (handled) e.preventDefault();
	}

	let currentTime = $state(0);
	let video: HTMLVideoElement | undefined = $state();
	let songsOpen = $state(false);

	const rerendering = $derived(
		s.renderMode === 'queued' || s.renderMode === 'running' || s.makeStep === 'failed'
	);
	const beat = $derived(s.manifest?.beat_sync ?? null);
	const trip = $derived(s.footage.find((f) => f.folder === s.folder)?.name ?? s.folder);
	const title = $derived(s.title || trip || 'Your video');
	const fileName = $derived(
		`${(title || 'vibecut').replace(/[^\w\- ]+/g, '').trim().replace(/\s+/g, '-') || 'vibecut'}.mp4`
	);
	const seconds = $derived(s.videoDuration ?? s.renderResult?.manifest_summary?.actual_duration_secs ?? 0);

	function seek(t: number) {
		if (video) video.currentTime = t;
		currentTime = t;
	}

	// The shortcuts list is a small non-modal dialog: focus moves into it
	// when it opens and, when it closes from inside, goes back to
	// whatever had it before (the Shortcuts button, or wherever "?" was
	// pressed).
	let helpEl: HTMLDivElement | undefined = $state();
	$effect(() => {
		const el = helpEl;
		if (!helpOpen || !el) return;
		const opener = document.activeElement instanceof HTMLElement ? document.activeElement : null;
		el.querySelector<HTMLElement>('button')?.focus();
		return () => {
			const at = document.activeElement;
			if (opener?.isConnected && (!at || at === document.body || el.contains(at))) opener.focus();
		};
	});
</script>

<svelte:window onkeydown={onKey} />

<ShareSheet bind:open={shareOpen} {title} {fileName} />

{#if helpOpen}
	<div
		class="help glass"
		role="dialog"
		aria-label="Keyboard shortcuts"
		tabindex="-1"
		bind:this={helpEl}
		onkeydown={(e) => {
			// Also from the checkbox, where the page shortcuts stand aside.
			if (e.key === 'Escape') {
				e.stopPropagation();
				helpOpen = false;
			}
		}}
	>
		<div class="help-head">
			<strong>Keyboard shortcuts</strong>
			<button class="btn ghost small" type="button" onclick={() => (helpOpen = false)}>Close</button>
		</div>
		<dl class:off={!keysOn}>
			{#each SHORTCUTS as [k, d] (k)}
				<dt><kbd>{k}</kbd></dt>
				<dd>{d}</dd>
			{/each}
		</dl>
		<label class="keys-toggle">
			<input type="checkbox" checked={keysOn} onchange={(e) => setKeysOn(e.currentTarget.checked)} />
			Single-key shortcuts
		</label>
	</div>
{/if}

<div class="studio">
	<div class="main">
		<header class="head">
			<div>
				<p class="eyebrow">{s.proposeResult?.vibe_name ?? 'Your video'} · {clock(seconds)}</p>
				<h1>{title}</h1>
			</div>
			<div class="head-actions">
				<button class="btn ghost small" type="button" aria-haspopup="dialog" aria-expanded={helpOpen} onclick={() => (helpOpen = !helpOpen)}
					>Shortcuts <kbd>?</kbd></button
				>
				{#if s.outputPath}
					<a class="btn ghost" href={outputFileUrl(s.outputPath)} download={fileName}>Download</a>
					<button class="btn primary download" type="button" onclick={() => (shareOpen = true)}>
						<svg width="18" height="18" viewBox="0 0 24 24" aria-hidden="true"
							><path
								d="M12 15V4m0 0L7.5 8.5M12 4l4.5 4.5M5 13v6h14v-6"
								fill="none"
								stroke="currentColor"
								stroke-width="2.2"
								stroke-linecap="round"
								stroke-linejoin="round"
							/></svg
						>
						Share
					</button>
				{/if}
			</div>
		</header>

		<div class="player-wrap">
			<Player bind:currentTime bind:video />
			{#if beat && !rerendering && beat.n_cuts > 0}
				{@const pctOn = Math.round((beat.within_tolerance / beat.n_cuts) * 100)}
				<div class="stamp" role="img" aria-label="{pctOn}% of cuts on the beat at {Math.round(beat.tempo_bpm)} BPM">
					<svg viewBox="0 0 100 100" aria-hidden="true">
						<defs>
							<path id="stamp-ring" d="M50,50 m-37,0 a37,37 0 1,1 74,0 a37,37 0 1,1 -74,0" />
						</defs>
						<circle cx="50" cy="50" r="47" class="stamp-bg" />
						<text class="stamp-text"
							><textPath href="#stamp-ring"
								>ON THE BEAT · {pctOn}% · ±{Math.round(beat.mean_abs_error_ms)} MS ·</textPath
							></text
						>
					</svg>
					<span class="stamp-bpm"><strong>{Math.round(beat.tempo_bpm)}</strong>BPM</span>
				</div>
			{/if}
			{#if rerendering}
				<div class="overlay">
					<CuttingRoom
						compact
						backLabel="Back to the last version"
						onBack={s.renderHistory.length > 0 ? () => s.openVersion(s.renderHistory.length - 1) : undefined}
					/>
				</div>
			{/if}
		</div>

		{#if s.segmentLayout.length > 0}
			<div class="glass panel"><BeatTimeline {currentTime} onSeek={seek} /></div>
		{:else if s.loadingManifest}
			<div class="glass panel dim">Loading the timeline…</div>
		{/if}

		<MomentInspector />
	</div>

	<aside class="rail">
		{#if beat}
			{@const allOn = beat.within_tolerance === beat.n_cuts}
			<div class="glass panel lock" class:all={allOn}>
				<BeatClock
					cuts={beat.cuts}
					intervalSecs={beat.beat_interval_secs}
					toleranceFrames={beat.tolerance_frames}
					fps={beat.fps}
				/>
				<div>
					<strong>{beat.within_tolerance}/{beat.n_cuts} cuts on the beat</strong>
					<span
						>{Math.round(beat.tempo_bpm)} BPM · average {beat.mean_abs_error_ms.toFixed(0)} ms off{#if s.effectiveTrack}
							· ♪ {s.effectiveTrack.title}{/if}</span
					>
				</div>
			</div>
		{/if}

		<div class="glass panel"><TweakPanel /></div>

		<div class="glass panel"><CommandBar /></div>

		<div class="glass panel">
			<button
				class="disclosure"
				type="button"
				aria-expanded={songsOpen}
				onclick={() => {
					songsOpen = !songsOpen;
					if (songsOpen && s.tracks.length === 0) void s.loadMusic();
				}}
			>
				<span class="eyebrow">Song</span>
				<span class="now serif">{s.effectiveTrack ? `♪ ${s.effectiveTrack.title}` : 'Original sound'}</span>
				<span class="chev" aria-hidden="true">{songsOpen ? '−' : '+'}</span>
			</button>
			{#if songsOpen}<SongPicker compact />{/if}
		</div>

		<div class="glass panel"><VersionsStrip /></div>
	</aside>
</div>

<style>
	.studio {
		display: grid;
		grid-template-columns: minmax(0, 1fr) 380px;
		gap: 22px;
		padding: 6px var(--gutter) 60px;
		max-width: 1500px;
		margin: 0 auto;
		align-items: start;
	}

	.main {
		display: flex;
		flex-direction: column;
		gap: 18px;
		min-width: 0;
	}

	.head {
		position: relative;
		z-index: 2; /* above the player's ambilight glow */
		display: flex;
		align-items: flex-end;
		justify-content: space-between;
		gap: 16px;
	}

	.head .eyebrow {
		margin: 0;
	}

	h1 {
		margin: 4px 0 0;
		font-size: var(--fs-2xl);
		letter-spacing: -0.035em;
		line-height: 1.05;
		animation: rise var(--dur-3) var(--ease-out) both;
	}

	.download {
		padding: 12px 20px;
	}

	.head-actions {
		display: flex;
		align-items: center;
		gap: 8px;
	}

	.head-actions a {
		text-decoration: none;
	}

	.small {
		padding: 6px 12px;
		font-size: var(--fs-xs);
	}

	kbd {
		font-family: var(--mono);
		font-size: 11px;
		padding: 1px 6px;
		border-radius: 5px;
		border: 1px solid var(--line-strong);
		background: rgba(255, 255, 255, 0.05);
	}

	.help {
		position: fixed;
		right: var(--gutter);
		top: 70px;
		z-index: 50;
		width: min(360px, calc(100vw - 32px));
		padding: 16px 18px;
		animation: rise var(--dur-2) var(--ease-out) both;
	}

	.help-head {
		display: flex;
		justify-content: space-between;
		align-items: center;
		margin-bottom: 8px;
	}

	.help dl {
		display: grid;
		grid-template-columns: auto 1fr;
		gap: 8px 14px;
		margin: 0;
		font-size: var(--fs-sm);
	}

	.help dt {
		text-align: right;
	}

	.help dd {
		margin: 0;
		color: var(--text-dim);
	}

	/* Dimmed, but still readable (≥ 4.5:1). */
	.help dl.off {
		opacity: 0.7;
	}

	.keys-toggle {
		display: flex;
		align-items: center;
		gap: 8px;
		margin-top: 12px;
		padding-top: 10px;
		border-top: 1px solid var(--line);
		font-size: var(--fs-sm);
		color: var(--text-dim);
		cursor: pointer;
	}

	@media (max-width: 640px) {
		.head {
			flex-direction: column;
			align-items: flex-start;
		}
		.head-actions .small {
			display: none;
		}
	}

	.player-wrap {
		position: relative;
		border-radius: var(--r-lg);
		animation: zoom 800ms var(--ease-out) both;
	}

	.overlay {
		position: absolute;
		inset: 0;
		border-radius: var(--r-lg);
		overflow: hidden;
		z-index: 4;
	}

	.panel {
		padding: 16px 18px;
	}

	.dim {
		color: var(--text-faint);
	}

	.rail {
		display: flex;
		flex-direction: column;
		gap: 14px;
		position: sticky;
		top: 74px;
		max-height: calc(100dvh - 90px);
		overflow-y: auto;
		scrollbar-width: thin;
		padding-bottom: 8px;
	}

	.lock {
		display: flex;
		align-items: center;
		gap: 14px;
		animation: rise var(--dur-3) 200ms var(--ease-out) both;
	}

	.lock.all {
		border-color: rgba(74, 222, 128, 0.4);
		background: linear-gradient(120deg, rgba(74, 222, 128, 0.12), rgba(56, 214, 255, 0.06));
	}

	.stamp {
		position: absolute;
		top: 14px;
		right: 14px;
		width: 104px;
		height: 104px;
		z-index: 3;
		display: grid;
		place-items: center;
		pointer-events: none;
		animation: stamp 700ms 400ms var(--ease-spring) both;
	}

	.stamp svg {
		position: absolute;
		inset: 0;
		animation: spin 18s linear infinite;
	}

	.stamp-bg {
		fill: rgba(7, 8, 12, 0.55);
		stroke: rgba(74, 222, 128, 0.7);
		stroke-width: 1.5;
	}

	.stamp-text {
		font-family: var(--mono);
		font-size: 8.6px;
		font-weight: 600;
		letter-spacing: 0.08em;
		fill: var(--good);
	}

	.stamp-bpm {
		display: flex;
		flex-direction: column;
		align-items: center;
		line-height: 1;
		font-size: 9px;
		font-weight: 700;
		letter-spacing: 0.12em;
		color: var(--text-dim);
		animation: pulse var(--beat) var(--ease-out) infinite;
	}

	.stamp-bpm strong {
		font-size: 24px;
		letter-spacing: -0.03em;
		color: var(--text);
	}

	@keyframes stamp {
		from {
			opacity: 0;
			transform: scale(1.8) rotate(-25deg);
		}
	}

	@keyframes spin {
		to {
			transform: rotate(360deg);
		}
	}

	@media (max-width: 640px) {
		.stamp {
			width: 76px;
			height: 76px;
			top: 8px;
			right: 8px;
		}
		.stamp-bpm strong {
			font-size: 18px;
		}
	}

	.lock strong {
		display: block;
	}

	.lock span {
		font-size: var(--fs-xs);
		color: var(--text-dim);
	}

	.disclosure {
		width: 100%;
		display: grid;
		grid-template-columns: auto 1fr auto;
		align-items: center;
		gap: 10px;
		background: none;
		border: none;
		padding: 0;
		cursor: pointer;
		text-align: left;
	}

	.now {
		font-size: 17px;
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}

	.chev {
		font-size: 20px;
		color: var(--text-dim);
	}

	.disclosure + :global(.act) {
		margin-top: 14px;
	}

	@media (max-width: 1060px) {
		.studio {
			grid-template-columns: minmax(0, 1fr);
		}
		.rail {
			position: static;
			max-height: none;
		}
	}

	@keyframes rise {
		from {
			opacity: 0;
			transform: translateY(12px);
		}
	}

	@keyframes zoom {
		from {
			opacity: 0;
			transform: scale(0.96);
			filter: blur(8px);
		}
	}

	@keyframes pulse {
		0% {
			transform: scale(1.2);
		}
		70%,
		100% {
			transform: scale(1);
		}
	}
</style>
