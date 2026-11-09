<script lang="ts">
	// A song's loudness sketch as rounded bars; the played part lights up.
	let {
		energy,
		progress = 0,
		active = false,
		height = 28
	}: { energy: number[]; progress?: number; active?: boolean; height?: number } = $props();

	const n = $derived(energy.length || 1);
</script>

<svg
	class="bars"
	class:active
	viewBox="0 0 {n * 4} {height}"
	preserveAspectRatio="none"
	style="height: {height}px"
	aria-hidden="true"
>
	{#each energy as e, i (i)}
		{@const h = Math.max(2, e * (height - 2))}
		<rect
			x={i * 4 + 0.6}
			y={(height - h) / 2}
			width="2.8"
			height={h}
			rx="1.4"
			class:played={i / n < progress}
		/>
	{/each}
</svg>

<style>
	.bars {
		width: 100%;
		display: block;
	}

	rect {
		fill: rgba(255, 255, 255, 0.22);
		transition: fill var(--dur-1) linear;
	}

	.active rect {
		fill: rgba(255, 255, 255, 0.32);
	}

	rect.played {
		fill: var(--magenta);
	}
</style>
