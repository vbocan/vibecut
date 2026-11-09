<script lang="ts">
	import { onMount } from 'svelte';
	import { appState as s } from '$lib/state.svelte';
	import TripPicker from './TripPicker.svelte';
	import VibePicker from './VibePicker.svelte';
	import SongPicker from './SongPicker.svelte';
	import CommandBar from './CommandBar.svelte';

	// Keyboard focus scrolls clear of the fixed "Make it" dock.
	onMount(() => {
		const html = document.documentElement;
		html.style.scrollPaddingBottom = 'calc(190px + env(safe-area-inset-bottom))';
		return () => (html.style.scrollPaddingBottom = '');
	});

	const ready = $derived(!!s.folder && !!s.effectiveVibe && !s.discovering);
	const vibe = $derived(s.vibes.find((v) => v.key === s.effectiveVibe));
	const track = $derived(s.effectiveTrack);
	const trip = $derived(s.footage.find((f) => f.folder === s.folder)?.name ?? s.folder);

	const missing = $derived(
		!s.folder ? 'Pick your footage' : s.discovering ? 'Reading your clips…' : !s.effectiveVibe ? 'Choose a vibe' : ''
	);

	function make() {
		if (!ready) return;
		void s.makeVideo();
	}
</script>

<div class="create">
	<header class="hero">
		<p class="eyebrow">Your footage · your music · zero editing</p>
		<h1>
			Your day, <span class="serif"><em>cut to</em></span> <span class="grad-text">the beat</span>.
		</h1>
		<p class="lede">
			Pick the footage, a vibe and a song. Vibecut finds the best moments in your clips and locks every
			cut to the music.
		</p>
	</header>

	<div class="acts">
		<div class="glass act-card"><TripPicker /></div>
		<div class="glass act-card"><VibePicker /></div>
		<div class="glass act-card"><SongPicker /></div>
	</div>

	<div class="glass act-card ai-card">
		<CommandBar />
	</div>

	<div class="make-dock" class:ready>
		<div class="summary" aria-live="polite">
			{#if ready}
				<span class="eyebrow">Ready</span>
				<span class="line">
					<strong>{trip}</strong>
					<span class="sep">·</span>
					<strong>{vibe?.name}</strong>
					{#if track}<span class="sep">·</span> <span class="serif">♪ {track.title}</span>{/if}
				</span>
			{:else}
				<span class="eyebrow">Next</span>
				<span class="line"><strong>{missing}</strong></span>
			{/if}
		</div>
		<label class="title-field">
			<span class="sr-only">Title on the opening card (optional)</span>
			<input placeholder="Title (optional)" bind:value={s.title} maxlength="60" />
		</label>
		<button class="make" type="button" disabled={!ready} onclick={make}>
			<span class="make-glow" aria-hidden="true"></span>
			<span class="make-label">Make my video</span>
			<svg viewBox="0 0 24 24" width="22" height="22" aria-hidden="true"
				><path
					d="M5 12h12m-5-6 6 6-6 6"
					fill="none"
					stroke="currentColor"
					stroke-width="2.4"
					stroke-linecap="round"
					stroke-linejoin="round"
				/></svg
			>
		</button>
	</div>
	{#if s.makeError}
		<p class="error" role="alert">{s.makeError}</p>
	{/if}
</div>

<style>
	.create {
		display: flex;
		flex-direction: column;
		gap: 22px;
		padding: 8px var(--gutter) 140px;
		max-width: 1280px;
		margin: 0 auto;
	}

	.hero {
		padding: clamp(10px, 4vh, 48px) 0 8px;
		max-width: 900px;
	}

	h1 {
		margin: 6px 0 10px;
		font-size: var(--fs-hero);
		line-height: 0.98;
		letter-spacing: -0.032em;
		font-weight: 800;
		animation: rise 900ms var(--ease-out) both;
	}

	h1 .serif {
		font-weight: 400;
		letter-spacing: -0.02em;
	}

	.lede {
		margin: 0;
		max-width: 620px;
		font-size: var(--fs-lg);
		color: var(--text-dim);
		animation: rise 900ms 120ms var(--ease-out) both;
	}

	.acts {
		display: grid;
		grid-template-columns: minmax(0, 1fr);
		gap: 18px;
	}

	.act-card {
		padding: clamp(16px, 2.2vw, 26px);
		animation: rise 800ms var(--ease-out) both;
	}

	.act-card:nth-child(2) {
		animation-delay: 90ms;
	}
	.act-card:nth-child(3) {
		animation-delay: 180ms;
	}

	.make-dock {
		position: fixed;
		left: 50%;
		bottom: max(18px, env(safe-area-inset-bottom));
		transform: translateX(-50%);
		z-index: 40;
		width: min(1100px, calc(100vw - 2 * var(--gutter)));
		display: flex;
		align-items: center;
		gap: 14px;
		padding: 10px 10px 10px 22px;
		border-radius: 999px;
		background: rgba(14, 16, 23, 0.82);
		border: 1px solid var(--line-strong);
		box-shadow: var(--shadow-2);
		backdrop-filter: var(--blur);
		-webkit-backdrop-filter: var(--blur);
		animation: dock 700ms 300ms var(--ease-out) both;
	}

	.summary {
		display: flex;
		flex-direction: column;
		min-width: 0;
		flex: 1;
	}

	.line {
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}

	.line .serif {
		font-size: 17px;
		color: var(--text-dim);
	}

	.sep {
		color: var(--text-faint);
		margin: 0 4px;
	}

	.title-field input {
		width: 170px;
		background: rgba(255, 255, 255, 0.05);
		border: 1px solid var(--line);
		border-radius: 999px;
		padding: 10px 16px;
	}

	.make {
		position: relative;
		display: inline-flex;
		align-items: center;
		gap: 10px;
		padding: 16px 28px;
		border-radius: 999px;
		border: none;
		font-weight: 800;
		font-size: 17px;
		letter-spacing: -0.01em;
		color: var(--on-cta);
		background: var(--grad-cta);
		cursor: pointer;
		overflow: hidden;
		isolation: isolate;
		transition:
			transform var(--dur-2) var(--ease-spring),
			filter var(--dur-2);
	}

	.make:disabled {
		background: rgba(255, 255, 255, 0.08);
		color: var(--text-faint);
		cursor: not-allowed;
	}

	.make:not(:disabled):hover {
		transform: scale(1.04);
		filter: saturate(1.15);
	}

	.make:not(:disabled):active {
		transform: scale(0.98);
	}

	.make-glow {
		position: absolute;
		inset: 0;
		z-index: -1;
		background: linear-gradient(110deg, transparent 30%, rgba(255, 255, 255, 0.55) 50%, transparent 70%);
		transform: translateX(-120%);
	}

	.ready .make-glow {
		animation: sheen 2.8s var(--ease-in-out) infinite;
	}

	.ready .make {
		box-shadow: 0 0 0 0 rgba(255, 61, 129, 0.6);
		animation: breathe var(--beat) var(--ease-out) infinite;
	}

	.error {
		color: var(--bad);
		text-align: center;
	}

	@media (max-width: 760px) {
		.make-dock {
			flex-wrap: wrap;
			border-radius: 26px;
			padding: 12px;
		}
		.summary {
			flex-basis: 100%;
			padding: 0 6px;
		}
		.title-field {
			flex: 1;
		}
		.title-field input {
			width: 100%;
		}
	}

	@media (max-width: 520px) {
		.title-field {
			display: none;
		}
		.make {
			flex: 1;
			justify-content: center;
		}
	}

	@keyframes rise {
		from {
			opacity: 0;
			transform: translateY(18px);
		}
	}

	@keyframes dock {
		from {
			opacity: 0;
			transform: translate(-50%, 30px);
		}
	}

	@keyframes sheen {
		0% {
			transform: translateX(-120%);
		}
		60%,
		100% {
			transform: translateX(120%);
		}
	}

	@keyframes breathe {
		0% {
			box-shadow: 0 0 0 0 rgba(255, 61, 129, 0.55);
		}
		85%,
		100% {
			box-shadow: 0 0 0 16px rgba(255, 61, 129, 0);
		}
	}
</style>
