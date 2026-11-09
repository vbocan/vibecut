<script lang="ts">
	import { appState as s } from '$lib/state.svelte';
	import { RECAP_LENGTHS, recapSummary, recapTiles, thumbKey } from '$lib/recap';
	import { radiogroup } from '$lib/radiogroup';

	// Shown once the server has a year with saved versions from enough
	// months (or trips): what it would include, before anything is made.
	const plan = $derived(s.recapPlan);
	const tiles = $derived(plan ? recapTiles(plan) : []);
	const unit = $derived(plan?.grouping === 'trip' ? 'trip' : 'month');
	const busy = $derived(s.makingRecap || s.rendering);
</script>

{#if plan?.eligible}
	<section class="glass recap" aria-labelledby="recap-h" aria-busy={s.loadingRecap}>
		<span class="aurora" aria-hidden="true"></span>
		<div class="head">
			<div class="titles">
				<p class="eyebrow">Made from the videos you kept</p>
				<h2 id="recap-h">Your <span class="grad-text">{s.recapYear}</span> recap</h2>
				<p class="lede">
					The best moment of each {unit}, in the order you filmed them — one song, every cut on the beat.
				</p>
			</div>
			<div class="controls">
				<div class="lengths" role="radiogroup" aria-label="Recap length" use:radiogroup>
					{#each RECAP_LENGTHS as secs (secs)}
						<button
							type="button"
							role="radio"
							aria-checked={s.recapLength === secs}
							class:on={s.recapLength === secs}
							disabled={busy}
							onclick={() => s.setRecapLength(secs)}>{secs} s</button
						>
					{/each}
				</div>
				<button class="btn primary make" type="button" disabled={busy} onclick={() => s.makeRecap()}>
					{s.makingRecap ? 'Planning…' : `Make my ${s.recapYear} recap`}
					<svg viewBox="0 0 24 24" width="18" height="18" aria-hidden="true"
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
		</div>

		<p class="summary" aria-live="polite">{recapSummary(plan)}</p>
		<ol class="tiles" aria-label="What your recap includes">
			{#each tiles as t, i (t.key)}
				{@const thumb = s.recapThumbs[thumbKey(t)]}
				<li class="tile" class:out={t.leftOut} style="--i: {i}">
					<span class="frame" class:loaded={!!thumb}>{#if thumb}<img src={thumb} alt="" />{/if}</span>
					<span class="shade"></span>
					{#if !t.leftOut}
						<span class="count" aria-hidden="true" title="{t.chosen} moment{t.chosen === 1 ? '' : 's'}"
							>{t.chosen}</span
						>
					{/if}
					<span class="meta">
						<span class="label">{t.label}</span>
						<span class="detail">{t.detail}</span>
						{#if !t.leftOut}<span class="sr-only">{t.chosen} moment{t.chosen === 1 ? '' : 's'}</span>{/if}
					</span>
				</li>
			{/each}
		</ol>
		{#each plan.notes as n (n)}<p class="note">{n}</p>{/each}
		{#if s.recapError}<p class="error" role="alert">{s.recapError}</p>{/if}
	</section>
{/if}

<style>
	.recap {
		position: relative;
		overflow: hidden;
		display: flex;
		flex-direction: column;
		gap: 14px;
		padding: clamp(16px, 2.2vw, 26px);
		animation: rise 800ms var(--ease-out) both;
	}

	/* A soft wash of the brand light, so the card reads as a gift rather
	   than another step of the flow. */
	.aurora {
		position: absolute;
		inset: -1px;
		z-index: -1;
		background:
			radial-gradient(60% 120% at 0% 0%, rgba(255, 122, 69, 0.16), transparent 60%),
			radial-gradient(50% 120% at 100% 0%, rgba(123, 92, 255, 0.18), transparent 65%);
		pointer-events: none;
	}

	.head {
		display: flex;
		align-items: flex-end;
		justify-content: space-between;
		gap: 18px;
		flex-wrap: wrap;
	}

	.titles {
		min-width: 0;
		max-width: 620px;
	}

	.eyebrow {
		margin: 0;
	}

	h2 {
		margin: 2px 0 4px;
		font-size: var(--fs-2xl);
		font-weight: 800;
		letter-spacing: -0.03em;
		line-height: 1.05;
	}

	.lede {
		margin: 0;
		color: var(--text-dim);
	}

	.controls {
		display: flex;
		align-items: center;
		gap: 12px;
		flex-wrap: wrap;
	}

	.lengths {
		display: inline-flex;
		padding: 3px;
		border-radius: 999px;
		border: 1px solid var(--line);
		background: rgba(255, 255, 255, 0.04);
	}

	.lengths button {
		border: none;
		background: transparent;
		color: var(--text-dim);
		padding: 7px 13px;
		border-radius: 999px;
		font-size: var(--fs-sm);
		font-weight: 600;
		cursor: pointer;
		transition:
			background var(--dur-1) var(--ease-out),
			color var(--dur-1) var(--ease-out);
	}

	.lengths button:hover:not(:disabled) {
		color: var(--text);
	}

	.lengths button.on {
		background: rgba(255, 255, 255, 0.12);
		color: var(--text);
	}

	.lengths button:disabled {
		cursor: not-allowed;
		opacity: 0.6;
	}

	.make {
		padding: 12px 20px;
		font-size: var(--fs-md);
		font-weight: 800;
	}

	.summary {
		margin: 0;
		font-size: var(--fs-sm);
		color: var(--text-faint);
	}

	.tiles {
		list-style: none;
		margin: 0;
		display: grid;
		grid-auto-flow: column;
		grid-auto-columns: minmax(148px, 176px);
		gap: 10px;
		overflow-x: auto;
		overflow-y: hidden;
		padding: 2px 2px 10px;
		scroll-snap-type: x proximity;
		scrollbar-width: thin;
	}

	.tile {
		position: relative;
		aspect-ratio: 16 / 10;
		border-radius: var(--r-md);
		overflow: hidden;
		border: 1px solid var(--line);
		background: var(--panel-alt);
		scroll-snap-align: start;
		animation: rise var(--dur-3) var(--ease-out) both;
		animation-delay: calc(var(--i, 0) * 60ms);
	}

	.tile.out {
		opacity: 0.5;
		border-style: dashed;
	}

	.frame {
		position: absolute;
		inset: 0;
		background: linear-gradient(100deg, #151923 30%, #1f2533 50%, #151923 70%);
		background-size: 300% 100%;
		animation: shimmer 1.4s linear infinite;
	}

	.frame.loaded,
	.out .frame {
		animation: none;
		background: linear-gradient(135deg, #1b2030, #10131b);
	}

	.frame img {
		width: 100%;
		height: 100%;
		object-fit: cover;
		animation: fade var(--dur-2) var(--ease-out) both;
	}

	.shade {
		position: absolute;
		inset: 0;
		background: linear-gradient(to top, rgba(5, 6, 10, 0.9), rgba(5, 6, 10, 0.05) 65%);
	}

	.count {
		position: absolute;
		top: 8px;
		right: 8px;
		min-width: 24px;
		height: 24px;
		padding: 0 7px;
		display: grid;
		place-items: center;
		border-radius: 999px;
		background: var(--grad-brand);
		color: #160a12;
		font-size: var(--fs-xs);
		font-weight: 800;
	}

	.meta {
		position: absolute;
		left: 12px;
		right: 12px;
		bottom: 9px;
		display: flex;
		flex-direction: column;
		min-width: 0;
	}

	.label {
		font-weight: 700;
		letter-spacing: -0.01em;
		line-height: 1.2;
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}

	.detail {
		font-size: var(--fs-xs);
		color: var(--text-dim);
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}

	.note {
		margin: 0;
		font-size: var(--fs-xs);
		color: var(--text-faint);
	}

	.error {
		margin: 0;
		color: var(--bad);
	}

	@media (max-width: 640px) {
		.controls {
			width: 100%;
		}
		.make {
			flex: 1;
		}
	}

	@keyframes rise {
		from {
			opacity: 0;
			transform: translateY(14px);
		}
	}

	@keyframes fade {
		from {
			opacity: 0;
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
</style>
