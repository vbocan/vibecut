<script lang="ts">
	import { appState as s } from '$lib/state.svelte';
	import { vibeLook, prettyDuration } from '$lib/vibes';
	import { radiogroup } from '$lib/radiogroup';

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
</script>

<section class="act" aria-labelledby="vibe-h">
	<div class="act-head">
		<span class="num" aria-hidden="true">2</span>
		<h2 id="vibe-h">Choose a vibe</h2>
	</div>

	{#if s.vibesError}
		<p class="error">{s.vibesError}</p>
	{/if}

	<div class="vibes" role="radiogroup" aria-labelledby="vibe-h" use:radiogroup>
		{#each s.vibes as v, i (v.key)}
			{@const look = vibeLook(v.key)}
			{@const isAuto = !s.selectedVibe && s.autoVibe === v.key}
			<!-- Named by the vibe alone; the rest of the card is its description. -->
			<button
				type="button"
				role="radio"
				aria-checked={s.effectiveVibe === v.key}
				aria-labelledby="vibe-{v.key}-name"
				aria-describedby="{isAuto ? `vibe-${v.key}-auto ` : ''}vibe-{v.key}-len vibe-{v.key}-tag vibe-{v.key}-best"
				class="vibe {look.motion}"
				class:selected={s.effectiveVibe === v.key}
				style="--a: {look.colors[0]}; --b: {look.colors[1]}; --c: {look.colors[2]}; --i: {i}"
				onclick={() => pick(v.key)}
			>
				<span class="art" aria-hidden="true">
					<span class="blob b1"></span>
					<span class="blob b2"></span>
					<span class="blob b3"></span>
					<span class="lines"></span>
				</span>
				{#if isAuto}
					<span class="auto-badge" id="vibe-{v.key}-auto"><span aria-hidden="true">✦</span> Picked for your footage</span>
				{/if}
				<span class="body">
					<span class="row">
						<span class="name" id="vibe-{v.key}-name">{v.name}</span>
						<span class="len" id="vibe-{v.key}-len">{prettyDuration(v.default_target_duration)}</span>
					</span>
					<span class="tagline" id="vibe-{v.key}-tag">{look.tagline}</span>
					<span class="best" id="vibe-{v.key}-best">
						{#each look.best as b (b)}<span>{b}</span>{/each}
					</span>
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

	.vibes {
		display: grid;
		grid-template-columns: repeat(auto-fill, minmax(230px, 1fr));
		gap: 14px;
	}

	.vibe {
		position: relative;
		display: flex;
		flex-direction: column;
		min-height: 196px;
		padding: 0;
		border-radius: var(--r-lg);
		overflow: hidden;
		border: 1px solid var(--line);
		background: #0d0f16;
		cursor: pointer;
		text-align: left;
		isolation: isolate;
		animation: rise var(--dur-3) var(--ease-out) both;
		animation-delay: calc(var(--i, 0) * 60ms);
		transition:
			transform var(--dur-2) var(--ease-out),
			box-shadow var(--dur-2) var(--ease-out),
			border-color var(--dur-2) var(--ease-out);
	}

	.vibe:hover {
		transform: translateY(-4px);
		border-color: var(--line-strong);
	}

	.vibe.selected {
		border-color: transparent;
		box-shadow:
			0 0 0 2px var(--b),
			0 22px 60px -18px var(--b);
		transform: translateY(-4px);
	}

	/* ---- living art ---- */
	.art {
		position: absolute;
		inset: 0;
		z-index: -1;
		background: radial-gradient(120% 90% at 0% 0%, color-mix(in srgb, var(--a) 40%, transparent), transparent 60%),
			radial-gradient(120% 90% at 100% 20%, color-mix(in srgb, var(--c) 45%, transparent), transparent 60%),
			#0d0f16;
		opacity: 0.85;
		transition: opacity var(--dur-2);
	}

	.vibe:hover .art,
	.vibe.selected .art {
		opacity: 1;
	}

	.art::after {
		content: '';
		position: absolute;
		inset: 0;
		background: linear-gradient(to top, rgba(8, 9, 14, 0.95) 18%, rgba(8, 9, 14, 0.35) 70%, rgba(8, 9, 14, 0.1));
	}

	.blob {
		position: absolute;
		border-radius: 50%;
		filter: blur(18px);
		opacity: 0.9;
	}

	.b1 {
		width: 120px;
		height: 120px;
		left: 8%;
		top: 6%;
		background: var(--a);
	}

	.b2 {
		width: 90px;
		height: 90px;
		right: 12%;
		top: 14%;
		background: var(--b);
	}

	.b3 {
		width: 70px;
		height: 70px;
		left: 46%;
		top: 30%;
		background: var(--c);
	}

	.lines {
		position: absolute;
		inset: 0;
		opacity: 0;
	}

	/* Personalities — slow at rest, livelier on hover/selection. */
	.bounce .b1 {
		animation: bounce 1.6s var(--ease-spring) infinite alternate;
	}
	.bounce .b2 {
		animation: bounce 1.2s var(--ease-spring) infinite alternate-reverse;
	}
	.bounce .b3 {
		animation: bounce 1.9s var(--ease-spring) infinite alternate;
	}

	.drift .b1,
	.drift .b2,
	.drift .b3 {
		animation: drift 12s var(--ease-in-out) infinite alternate;
	}
	.drift .b2 {
		animation-duration: 16s;
	}

	.glide .lines {
		opacity: 0.5;
		background: repeating-linear-gradient(
			180deg,
			transparent 0 18px,
			color-mix(in srgb, var(--a) 35%, transparent) 18px 19px
		);
		mask-image: linear-gradient(to bottom, transparent, black 40%, transparent 80%);
		animation: glide 9s linear infinite;
	}
	.glide .b1,
	.glide .b2 {
		animation: drift 18s var(--ease-in-out) infinite alternate;
	}

	.strobe .lines {
		opacity: 0.55;
		background: repeating-linear-gradient(
			115deg,
			transparent 0 22px,
			color-mix(in srgb, var(--a) 55%, transparent) 22px 30px
		);
		animation: sweep 1.1s linear infinite;
	}
	.strobe .b1,
	.strobe .b2,
	.strobe .b3 {
		animation: flash 0.9s steps(2) infinite;
	}
	.strobe .b2 {
		animation-delay: 0.3s;
	}

	.calm .b1,
	.calm .b2,
	.calm .b3 {
		animation: breathe 6s var(--ease-in-out) infinite alternate;
		opacity: 0.6;
	}

	.float .b1,
	.float .b2,
	.float .b3 {
		animation: float 10s var(--ease-in-out) infinite;
	}
	.float .b2 {
		animation-delay: -3s;
	}
	.float .b3 {
		animation-delay: -6s;
	}

	.vibe:not(:hover):not(.selected) .blob,
	.vibe:not(:hover):not(.selected) .lines {
		animation-play-state: paused;
	}

	/* ---- copy ---- */
	.body {
		margin-top: auto;
		display: flex;
		flex-direction: column;
		gap: 6px;
		padding: 18px;
	}

	.row {
		display: flex;
		align-items: baseline;
		justify-content: space-between;
		gap: 8px;
	}

	.name {
		font-size: 21px;
		font-weight: 700;
		letter-spacing: -0.02em;
	}

	.len {
		font-family: var(--mono);
		font-size: var(--fs-xs);
		color: var(--text-dim);
	}

	.tagline {
		font-family: var(--serif);
		font-size: 18px;
		line-height: 1.25;
		color: #e9e4ff;
	}

	.best {
		display: flex;
		flex-wrap: wrap;
		gap: 6px;
		margin-top: 4px;
	}

	.best span {
		font-size: 11.5px;
		padding: 2px 8px;
		border-radius: 999px;
		background: rgba(255, 255, 255, 0.08);
		color: var(--text-dim);
	}

	.auto-badge {
		position: absolute;
		top: 12px;
		left: 12px;
		padding: 3px 10px;
		border-radius: 999px;
		font-size: 11.5px;
		font-weight: 700;
		background: rgba(7, 8, 12, 0.6);
		border: 1px solid rgba(255, 255, 255, 0.25);
		backdrop-filter: blur(6px);
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

	/* Phones: a compact two-up grid instead of six tall tiles. */
	@media (max-width: 640px) {
		.vibes {
			grid-template-columns: repeat(2, minmax(0, 1fr));
			gap: 10px;
		}
		.vibe {
			min-height: 150px;
		}
		.body {
			padding: 12px;
			gap: 4px;
		}
		.name {
			font-size: 16px;
		}
		.len,
		.best {
			display: none;
		}
		.tagline {
			font-size: 14.5px;
		}
	}

	@keyframes rise {
		from {
			opacity: 0;
			transform: translateY(14px) scale(0.98);
		}
	}
	@keyframes bounce {
		from {
			transform: translateY(0) scale(1);
		}
		to {
			transform: translateY(26px) scale(1.15);
		}
	}
	@keyframes drift {
		from {
			transform: translate(0, 0) scale(1);
		}
		to {
			transform: translate(40px, 20px) scale(1.3);
		}
	}
	@keyframes glide {
		from {
			background-position: 0 0;
		}
		to {
			background-position: 0 190px;
		}
	}
	@keyframes sweep {
		from {
			background-position: 0 0;
		}
		to {
			background-position: 140px 0;
		}
	}
	@keyframes flash {
		50% {
			opacity: 0.35;
		}
	}
	@keyframes breathe {
		to {
			transform: scale(1.25);
			opacity: 0.85;
		}
	}
	@keyframes float {
		0% {
			transform: translateY(40px) scale(0.8);
			opacity: 0;
		}
		30% {
			opacity: 0.9;
		}
		100% {
			transform: translateY(-70px) scale(1.2);
			opacity: 0;
		}
	}
</style>
