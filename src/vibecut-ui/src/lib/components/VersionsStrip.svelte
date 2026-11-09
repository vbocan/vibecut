<script lang="ts">
	import { appState as s } from '$lib/state.svelte';
	import { outputFileUrl } from '$lib/api';
	import { clock } from '$lib/vibes';
	import { radiogroup } from '$lib/radiogroup';

	let comparing = $state(false);
	let wipe = $state(50);
	let a: HTMLVideoElement | undefined = $state();
	let b: HTMLVideoElement | undefined = $state();

	const current = $derived(s.outputPath);
	const A = $derived(s.compareA !== null ? s.renderHistory[s.compareA] : null);
	const B = $derived(s.compareB !== null ? s.renderHistory[s.compareB] : null);

	// Keep B locked to A: play, pause and seek together.
	function sync() {
		if (!a || !b) return;
		if (Math.abs(b.currentTime - a.currentTime) > 0.08) b.currentTime = a.currentTime;
		if (a.paused !== b.paused) (a.paused ? b.pause() : void b.play());
	}

	function toggleCompare() {
		comparing = !comparing;
		if (comparing && s.renderHistory.length >= 2) {
			s.compareB = s.renderHistory.length - 1;
			s.compareA = s.renderHistory.length - 2;
		}
	}
</script>

<div class="versions">
	<div class="head">
		<p class="eyebrow">Versions</p>
		{#if s.renderHistory.length >= 2}
			<button
				class="btn ghost small"
				type="button"
				aria-expanded={comparing}
				aria-controls={comparing ? 'compare-ab' : undefined}
				onclick={toggleCompare}
			>
				{comparing ? 'Done comparing' : 'Compare A/B'}
			</button>
		{/if}
	</div>

	{#if s.renderHistory.length === 0}
		<p class="dim">Each change you make keeps a version here.</p>
	{:else}
		<!-- One tab stop. Opening a version reloads the player and the
		     timeline, so arrows only move focus and Space / Enter opens. -->
		<ol class="strip" role="radiogroup" aria-label="Versions" use:radiogroup={{ selectOnMove: false }}>
			{#each s.renderHistory as v, i (v.output_path)}
				<li role="none">
					<button
						type="button"
						class="ver"
						role="radio"
						class:on={v.output_path === current}
						aria-checked={v.output_path === current}
						onclick={() => s.openVersion(i)}
					>
						<span class="label">{v.label}</span>
						<span class="meta">{v.vibe ?? ''}{v.duration_secs ? ` · ${clock(v.duration_secs)}` : ''}</span>
						{#if v.song}<span class="song serif">♪ {v.song}</span>{/if}
						{#if v.beats_locked}<span class="beat">{v.beats_locked}</span>{/if}
					</button>
				</li>
			{/each}
		</ol>
	{/if}

	{#if comparing && A && B}
		<div class="compare" id="compare-ab">
			<div class="pickers">
				<label
					>A <select bind:value={s.compareA} aria-label="Version A">
						{#each s.renderHistory as r, i (i)}<option value={i}>{r.label}</option>{/each}
					</select></label
				>
				<label
					>B <select bind:value={s.compareB} aria-label="Version B">
						{#each s.renderHistory as r, i (i)}<option value={i}>{r.label}</option>{/each}
					</select></label
				>
			</div>
			<div class="stage">
				<!-- svelte-ignore a11y_media_has_caption -->
				<video bind:this={b} src={outputFileUrl(B.output_path)} muted playsinline aria-hidden="true"></video>
				<!-- svelte-ignore a11y_media_has_caption -->
				<video
					bind:this={a}
					class="top"
					aria-label="Version A over version B"
					src={outputFileUrl(A.output_path)}
					controls
					playsinline
					style="clip-path: inset(0 {100 - wipe}% 0 0)"
					ontimeupdate={sync}
					onplay={sync}
					onpause={sync}
					onseeked={sync}
				></video>
				<span class="divider" style="left: {wipe}%" aria-hidden="true"></span>
				<span class="badge a">A · {A.label}</span>
				<span class="badge b">B · {B.label}</span>
			</div>
			<label class="wipe">
				<span class="sr-only">Wipe between A and B</span>
				<input type="range" min="0" max="100" bind:value={wipe} />
			</label>
		</div>
	{/if}
</div>

<style>
	.versions {
		display: flex;
		flex-direction: column;
		gap: 10px;
	}

	.head {
		display: flex;
		align-items: center;
		justify-content: space-between;
	}

	.eyebrow {
		margin: 0;
	}

	.small {
		padding: 5px 10px;
		font-size: var(--fs-xs);
	}

	.dim {
		margin: 0;
		color: var(--text-faint);
		font-size: var(--fs-sm);
	}

	.strip {
		list-style: none;
		margin: 0;
		padding: 0;
		display: flex;
		flex-direction: column;
		gap: 6px;
		max-height: 260px;
		overflow-y: auto;
		scrollbar-width: thin;
	}

	.ver {
		width: 100%;
		display: grid;
		grid-template-columns: 1fr auto;
		gap: 2px 10px;
		text-align: left;
		padding: 10px 12px;
		border-radius: var(--r-sm);
		border: 1px solid var(--line);
		background: rgba(255, 255, 255, 0.025);
		cursor: pointer;
	}

	.ver:hover {
		background: rgba(255, 255, 255, 0.06);
	}

	/* The list scrolls, which would clip an outside focus ring. */
	.ver:focus-visible {
		outline-offset: -2px;
	}

	.ver.on {
		border-color: rgba(255, 61, 129, 0.6);
		background: rgba(255, 61, 129, 0.08);
	}

	.label {
		font-weight: 700;
	}

	.meta {
		font-size: var(--fs-xs);
		color: var(--text-faint);
		text-align: right;
	}

	.song {
		font-size: 15px;
		color: var(--text-dim);
	}

	.beat {
		font-size: var(--fs-xs);
		color: var(--good);
		text-align: right;
	}

	.compare {
		display: flex;
		flex-direction: column;
		gap: 8px;
	}

	.pickers {
		display: flex;
		gap: 10px;
		font-size: var(--fs-sm);
	}

	select {
		background: var(--panel-alt);
		border: 1px solid var(--line-strong);
		border-radius: 8px;
		padding: 4px 6px;
	}

	.stage {
		position: relative;
		aspect-ratio: 16 / 9;
		border-radius: var(--r-md);
		overflow: hidden;
		background: #000;
	}

	.stage video {
		position: absolute;
		inset: 0;
		width: 100%;
		height: 100%;
	}

	.divider {
		position: absolute;
		top: 0;
		bottom: 0;
		width: 2px;
		margin-left: -1px;
		background: #fff;
		box-shadow: 0 0 10px #fff;
		pointer-events: none;
	}

	.badge {
		position: absolute;
		top: 8px;
		font-size: 11px;
		font-weight: 700;
		padding: 2px 8px;
		border-radius: 999px;
		background: rgba(0, 0, 0, 0.6);
	}

	.badge.a {
		left: 8px;
	}

	.badge.b {
		right: 8px;
	}

	.wipe input {
		width: 100%;
		accent-color: var(--magenta);
	}
</style>
