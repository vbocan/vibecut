<script lang="ts">
	import { appState as s } from '$lib/state.svelte';
	import { beatTicks, clipsCut, storyboardCaption, storyboardTiles } from '$lib/storyboard';

	// The edit before the encode: the planned shots on their beat grid,
	// shown the moment the plan arrives — seconds into the render. Shots
	// light up as the render cuts them.
	let { compact = false }: { compact?: boolean } = $props();

	const plan = $derived(s.plan);
	const tiles = $derived(plan ? storyboardTiles(plan) : []);
	const ticks = $derived(plan ? beatTicks(plan, compact ? 80 : 160) : []);
	const nClips = $derived(plan?.timeline.clips.length ?? 0);
	const cut = $derived(clipsCut(s.renderProgress, nClips));
	const caption = $derived(plan ? storyboardCaption(plan) : '');
	const title = $derived(s.title || plan?.project_name || 'Title');
</script>

{#if plan}
	<figure class="storyboard" class:compact>
		<figcaption>
			<span class="eyebrow">The edit</span>
			<span class="mono facts">{caption}{#if !compact && nClips > 0}&nbsp;· {cut} of {nClips} cut{/if}</span>
		</figcaption>
		<div
			class="strip"
			role="img"
			aria-label="Storyboard of the planned edit: {caption}. {cut} of {nClips} shots cut so far."
		>
			{#each tiles as t, i (`${t.kind}-${t.clip ?? ''}`)}
				{@const src = t.clip !== null ? s.planThumbs[t.clip] : undefined}
				<div
					class="tile {t.kind}"
					class:burst={t.burst}
					class:ramp={t.ramp}
					class:done={t.kind === 'clip' && t.clip !== null && t.clip < cut}
					class:loaded={!!src}
					style="left: {t.left}%; width: {t.width}%; --i: {i}"
				>
					{#if src}
						<img {src} alt="" />
					{:else if t.kind !== 'clip'}
						<span class="card-label">{t.kind === 'intro' ? title : 'End'}</span>
					{/if}
				</div>
			{/each}
		</div>
		<div class="beats" aria-hidden="true">
			{#each ticks as b, i (i)}
				<i class:bar={b.bar} style="left: {b.left}%"></i>
			{/each}
		</div>
	</figure>
{/if}

<style>
	.storyboard {
		width: 100%;
		margin: 6px 0 0;
		display: flex;
		flex-direction: column;
		gap: 6px;
		animation: arrive 600ms var(--ease-out) both;
	}

	figcaption {
		display: flex;
		align-items: baseline;
		justify-content: space-between;
		gap: 12px;
	}

	figcaption .eyebrow {
		margin: 0;
	}

	.facts {
		font-size: var(--fs-xs);
		color: var(--text-faint);
		font-variant-numeric: tabular-nums;
	}

	.strip {
		position: relative;
		height: 74px;
		border-radius: var(--r-sm);
		overflow: hidden;
		background: rgba(255, 255, 255, 0.04);
		box-shadow: inset 0 0 0 1px var(--line);
	}

	.compact .strip {
		height: 42px;
	}

	.tile {
		position: absolute;
		top: 0;
		bottom: 0;
		overflow: hidden;
		background: linear-gradient(180deg, rgba(255, 255, 255, 0.08), rgba(255, 255, 255, 0.03));
		/* a hairline between shots */
		box-shadow: inset -1px 0 0 rgba(7, 8, 12, 0.9);
		opacity: 0.5;
		filter: saturate(0.35);
		transition:
			opacity 500ms var(--ease-out),
			filter 500ms var(--ease-out);
		animation: tile-in 420ms var(--ease-out) backwards;
		animation-delay: calc(var(--i) * 28ms);
	}

	.tile img {
		width: 100%;
		height: 100%;
		object-fit: cover;
		display: block;
		animation: fade 400ms var(--ease-out) both;
	}

	.tile.done,
	.tile.intro,
	.tile.outro {
		opacity: 1;
		filter: none;
	}

	/* A cut shot gets the brand underline. */
	.tile.done::after {
		content: '';
		position: absolute;
		left: 0;
		right: 0;
		bottom: 0;
		height: 3px;
		background: var(--grad-brand);
	}

	/* The beat-burst: rapid cuts on every beat. */
	.tile.burst::before {
		content: '';
		position: absolute;
		left: 0;
		right: 0;
		top: 0;
		height: 3px;
		background: var(--magenta);
		z-index: 1;
	}

	.tile.intro,
	.tile.outro {
		display: grid;
		place-items: center;
		background: #0a0a0c;
	}

	.card-label {
		padding: 0 6px;
		font-size: 11px;
		font-weight: 700;
		letter-spacing: -0.01em;
		color: var(--text-dim);
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
		max-width: 100%;
	}

	.compact .card-label {
		font-size: 10px;
	}

	.beats {
		position: relative;
		height: 10px;
	}

	.beats i {
		position: absolute;
		top: 0;
		width: 1px;
		height: 5px;
		background: rgba(255, 255, 255, 0.28);
	}

	.beats i.bar {
		height: 10px;
		background: var(--cyan);
		opacity: 0.8;
	}

	@keyframes arrive {
		from {
			opacity: 0;
			transform: translateY(10px);
		}
	}

	@keyframes tile-in {
		from {
			opacity: 0;
			transform: scaleY(0.6);
		}
	}

	@keyframes fade {
		from {
			opacity: 0;
		}
	}

	@media (prefers-reduced-motion: reduce) {
		.storyboard,
		.tile,
		.tile img {
			animation: none;
		}
	}
</style>
