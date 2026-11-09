<script lang="ts">
	// One revolution = one beat. Each cut is a dot at its offset from the
	// nearest beat: on-beat cuts cluster at 12 o'clock inside the lime
	// tolerance wedge; randomly placed cuts would scatter all around.
	import type { CutSync } from '$lib/types/generated/CutSync';

	let {
		cuts,
		intervalSecs,
		toleranceFrames,
		fps,
		size = 76
	}: { cuts: CutSync[]; intervalSecs: number; toleranceFrames: number; fps: number; size?: number } = $props();

	const r = 30;
	const c = 38;
	const intervalMs = $derived(intervalSecs * 1000);
	const tolDeg = $derived(Math.min(170, ((toleranceFrames * (1000 / Math.max(1, fps))) / intervalMs) * 360));

	function point(deg: number, radius: number) {
		const a = ((deg - 90) * Math.PI) / 180;
		return { x: c + radius * Math.cos(a), y: c + radius * Math.sin(a) };
	}

	const wedge = $derived.by(() => {
		const a = point(-tolDeg, r + 4);
		const b = point(tolDeg, r + 4);
		const large = tolDeg * 2 > 180 ? 1 : 0;
		return `M ${c} ${c} L ${a.x} ${a.y} A ${r + 4} ${r + 4} 0 ${large} 1 ${b.x} ${b.y} Z`;
	});

	const dots = $derived(
		cuts.map((cut, i) => {
			const deg = (cut.error_ms / intervalMs) * 360;
			return { ...point(deg, r - 4 - (i % 4) * 4), on: Math.abs(cut.error_frames) <= toleranceFrames };
		})
	);
</script>

<svg
	class="clock"
	viewBox="0 0 76 76"
	width={size}
	height={size}
	role="img"
	aria-label="Beat clock: {cuts.filter((x) => Math.abs(x.error_frames) <= toleranceFrames).length} of {cuts.length} cuts within the beat tolerance"
>
	<circle cx={c} cy={c} r={r + 4} class="rim" />
	<path d={wedge} class="wedge" />
	<line x1={c} y1={c - r - 6} x2={c} y2={c - r + 2} class="noon" />
	{#each dots as d, i (i)}
		<circle cx={d.x} cy={d.y} r="2.4" class="dot" class:on={d.on} style="--i: {i}" />
	{/each}
	<circle cx={c} cy={c} r="3" class="hub" />
</svg>

<style>
	.clock {
		flex-shrink: 0;
		overflow: visible;
	}

	.rim {
		fill: rgba(255, 255, 255, 0.03);
		stroke: rgba(255, 255, 255, 0.14);
		stroke-dasharray: 2 3;
	}

	.wedge {
		fill: rgba(74, 222, 128, 0.18);
		stroke: rgba(74, 222, 128, 0.45);
		stroke-width: 0.6;
	}

	.noon {
		stroke: var(--good);
		stroke-width: 2;
		stroke-linecap: round;
	}

	.dot {
		fill: var(--bad);
		animation: land 700ms var(--ease-spring) both;
		animation-delay: calc(var(--i) * 40ms);
	}

	.dot.on {
		fill: var(--good);
		filter: drop-shadow(0 0 3px rgba(74, 222, 128, 0.9));
	}

	.hub {
		fill: var(--text);
	}

	@keyframes land {
		from {
			opacity: 0;
			transform: scale(0.2);
			transform-origin: 38px 38px;
		}
	}
</style>
