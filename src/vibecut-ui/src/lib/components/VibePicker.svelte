<script lang="ts">
	import { appState as s } from '$lib/state.svelte';
	import { vibeLook, prettyDuration } from '$lib/vibes';
	import { radiogroup } from '$lib/radiogroup';
	import VibeIcon from './VibeIcon.svelte';

	function pick(key: string) {
		s.selectedVibe = key;
		s.persist();
	}

	const selected = $derived(s.vibes.find((v) => v.key === s.effectiveVibe) ?? null);

	const lengths = $derived([
		{ label: 'Auto', value: 'auto', note: `${s.autoLengthSecs} s` },
		{ label: 'Reel', value: '30s', note: '30 s' },
		{ label: 'Short', value: '1m', note: '1 min' },
		{
			label: 'Vibe default',
			value: '',
			note: selected ? prettyDuration(selected.default_target_duration) : '—'
		},
		{ label: 'Long', value: '5m', note: '5 min' }
	]);

	/** Cut strip: shot widths in % of the strip. */
	function strip(cuts: number[]): number[] {
		const total = cuts.reduce((a, b) => a + b, 0) || 1;
		return cuts.map((c) => (c / total) * 100);
	}
</script>

<section class="act" aria-labelledby="vibe-h">
	<div class="act-head">
		<span class="num" aria-hidden="true">2</span>
		<h2 id="vibe-h">Choose a vibe</h2>
		<span class="hint">How your video is cut and how it sounds.</span>
	</div>

	{#if s.vibesError}
		<p class="error">{s.vibesError}</p>
	{/if}

	<div class="vibes" role="radiogroup" aria-labelledby="vibe-h" use:radiogroup>
		{#each s.vibes as v, i (v.key)}
			{@const look = vibeLook(v.key)}
			{@const isAuto = !s.selectedVibe && s.autoVibe === v.key}
			{@const on = s.effectiveVibe === v.key}
			<!-- Named by the vibe alone; the rest of the card is its description. -->
			<button
				type="button"
				role="radio"
				aria-checked={on}
				aria-labelledby="vibe-{v.key}-name"
				aria-describedby="{isAuto ? `vibe-${v.key}-auto ` : ''}vibe-{v.key}-tag vibe-{v.key}-facts vibe-{v.key}-best"
				class="vibe"
				class:selected={on}
				style="--a: {look.colors[0]}; --b: {look.colors[1]}; --c: {look.colors[2]}; --i: {i}"
				onclick={() => pick(v.key)}
			>
				<span class="top">
					<span class="icon"><VibeIcon icon={look.icon} /></span>
					<span class="titles">
						<span class="name" id="vibe-{v.key}-name">{v.name}</span>
						<span class="tagline" id="vibe-{v.key}-tag">{look.tagline}</span>
					</span>
				</span>

				<!-- The cut rhythm: one block per shot, as long as the shot. -->
				<span class="cuts" aria-hidden="true">
					{#each strip(look.cuts) as w, k (k)}<span class="shot" style="flex-basis: {w}%; --k: {k % 3}"></span>{/each}
				</span>

				<span class="facts" id="vibe-{v.key}-facts">
					<span class="fact"><span class="k">Cuts</span> {look.pace}</span>
					<span class="fact"><span class="k">Sound</span> {look.sound}</span>
					<span class="fact"><span class="k">Length</span> about {prettyDuration(v.default_target_duration)}</span>
				</span>

				<span class="foot">
					<span class="best" id="vibe-{v.key}-best">Good for {look.best.join(', ').toLowerCase()}</span>
					{#if on}
						<span class="state on-state" aria-hidden="true">✓ Selected</span>
					{:else if isAuto}
						<span class="state" id="vibe-{v.key}-auto"><span aria-hidden="true">✦</span> Suits your footage</span>
					{/if}
				</span>
			</button>
		{/each}
	</div>

	{#if selected}
		<div class="lengths" role="radiogroup" aria-labelledby="length-h" use:radiogroup>
			<span class="eyebrow" id="length-h">Length</span>
			{#each lengths as l (l.label)}
				<button
					type="button"
					role="radio"
					aria-checked={s.targetDuration === l.value}
					class="len-chip"
					class:on={s.targetDuration === l.value}
					onclick={() => (s.targetDuration = l.value)}
				>
					{l.label} <span>{l.note}</span>
				</button>
			{/each}
		</div>
	{/if}
</section>

<style>
	.act {
		display: flex;
		flex-direction: column;
		gap: 16px;
	}

	.act-head {
		display: flex;
		align-items: center;
		flex-wrap: wrap;
		gap: 12px;
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

	.vibes {
		display: grid;
		grid-template-columns: repeat(auto-fill, minmax(260px, 1fr));
		gap: 14px;
	}

	.vibe {
		position: relative;
		display: flex;
		flex-direction: column;
		gap: 14px;
		padding: 16px;
		border-radius: var(--r-lg);
		border: 1px solid var(--line);
		/* A thin band of the vibe's colour along the top. */
		background:
			linear-gradient(90deg, var(--a), var(--b), var(--c)) top / 100% 3px no-repeat,
			linear-gradient(180deg, color-mix(in srgb, var(--b) 10%, #10121a), #0d0f16 70%);
		color: var(--text);
		cursor: pointer;
		text-align: left;
		animation: rise var(--dur-3) var(--ease-out) both;
		animation-delay: calc(var(--i, 0) * 50ms);
		transition:
			transform var(--dur-2) var(--ease-out),
			box-shadow var(--dur-2) var(--ease-out),
			border-color var(--dur-2) var(--ease-out);
	}

	.vibe:hover {
		transform: translateY(-2px);
		border-color: var(--line-strong);
	}

	.vibe.selected {
		border-color: var(--b);
		box-shadow:
			0 0 0 1px var(--b),
			0 18px 50px -20px var(--b);
	}

	.top {
		display: grid;
		grid-template-columns: auto minmax(0, 1fr);
		gap: 4px 12px;
		align-items: start;
	}

	.icon {
		display: grid;
		place-items: center;
		width: 44px;
		height: 44px;
		border-radius: 12px;
		color: var(--a);
		background: color-mix(in srgb, var(--b) 16%, transparent);
		border: 1px solid color-mix(in srgb, var(--b) 40%, transparent);
	}

	.titles {
		display: flex;
		flex-direction: column;
		gap: 3px;
		min-width: 0;
	}

	.name {
		font-size: 18px;
		font-weight: 700;
		letter-spacing: -0.01em;
	}

	.tagline {
		font-size: var(--fs-sm);
		color: var(--text-dim);
		line-height: 1.4;
	}

	.state {
		flex-shrink: 0;
		padding: 2px 10px;
		border-radius: 999px;
		font-size: var(--fs-xs);
		font-weight: 700;
		border: 1px solid var(--line-strong);
		color: var(--text-dim);
	}

	.on-state {
		color: var(--ink);
		background: var(--b);
		border-color: var(--b);
	}

	.cuts {
		display: flex;
		gap: 3px;
		height: 14px;
	}

	.shot {
		flex-grow: 0;
		flex-shrink: 1;
		min-width: 3px;
		border-radius: 3px;
		background: var(--b);
		opacity: calc(0.55 + var(--k) * 0.18);
	}

	.facts {
		display: flex;
		flex-direction: column;
		gap: 4px;
		font-size: var(--fs-sm);
		color: var(--text);
	}

	.fact .k {
		display: inline-block;
		width: 58px;
		color: var(--text-faint);
		font-size: var(--fs-xs);
		font-weight: 700;
		text-transform: uppercase;
		letter-spacing: 0.06em;
	}

	.foot {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 8px;
		margin-top: auto;
		padding-top: 10px;
		border-top: 1px solid var(--line);
		min-height: 22px;
	}

	.best {
		font-size: var(--fs-xs);
		color: var(--text-dim);
	}

	.lengths {
		display: flex;
		align-items: center;
		flex-wrap: wrap;
		gap: 8px;
		animation: rise var(--dur-2) var(--ease-out) both;
	}

	.lengths .eyebrow {
		margin-right: 4px;
	}

	.len-chip {
		border: 1px solid var(--line-strong);
		background: rgba(255, 255, 255, 0.03);
		border-radius: 999px;
		padding: 7px 14px;
		font-weight: 600;
		font-size: var(--fs-sm);
		cursor: pointer;
		transition: all var(--dur-1) var(--ease-out);
	}

	.len-chip span {
		color: var(--text-faint);
		font-weight: 500;
		margin-left: 4px;
	}

	.len-chip.on {
		background: var(--text);
		color: var(--ink);
		border-color: var(--text);
	}

	.len-chip.on span {
		color: #4a4f5c;
	}

	.error {
		color: var(--bad);
	}

	@media (max-width: 640px) {
		.vibes {
			grid-template-columns: 1fr;
			gap: 10px;
		}
		.vibe {
			padding: 14px;
			gap: 10px;
		}
	}

	@media (prefers-reduced-motion: reduce) {
		.vibe {
			animation: none;
			transition: none;
		}
	}

	@keyframes rise {
		from {
			opacity: 0;
			transform: translateY(14px) scale(0.98);
		}
	}
</style>
