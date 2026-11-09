<script lang="ts">
	import { appState as s } from '$lib/state.svelte';
	import { outputFileUrl, inspectAt } from '$lib/api';
	import { sourceColor } from '$lib/colors';
	import { clock } from '$lib/vibes';
	import { radiogroup } from '$lib/radiogroup';

	let {
		currentTime = 0,
		onSeek
	}: { currentTime?: number; onSeek: (t: number) => void } = $props();

	let wrap: HTMLDivElement | undefined = $state();
	let canvas: HTMLCanvasElement | undefined = $state();
	let width = $state(0);
	let peaks = $state<Float32Array | null>(null);
	let thumbs = $state<Record<number, string>>({});
	let hoverT = $state<number | null>(null);

	const layout = $derived(s.segmentLayout);
	const duration = $derived(s.videoDuration ?? layout.at(-1)?.endSecs ?? 0);
	const sourceIds = $derived(s.manifest?.sources.map((x) => x.id) ?? []);
	const cuts = $derived(s.manifest?.beat_sync?.cuts ?? []);
	// Title / end cards sit outside the footage segments.
	const introEnd = $derived(cuts.find((c) => c.kind === 'intro_to_body')?.output_secs ?? 0);
	const outroStart = $derived(cuts.find((c) => c.kind === 'body_to_outro')?.output_secs ?? null);
	// The single strongest moment (not the burst) gets a pin.
	const hero = $derived.by(() => {
		let best: (typeof layout)[number] | null = null;
		for (const g of layout) {
			if (g.segment.source_id.startsWith('burst:') || g.segment.score === null) continue;
			if (!best || (g.segment.score ?? 0) > (best.segment.score ?? 0)) best = g;
		}
		return best;
	});
	const tolerance = $derived(s.manifest?.beat_sync?.tolerance_frames ?? 3);

	const pctOf = (t: number) => (duration > 0 ? (t / duration) * 100 : 0);

	// ---- waveform: decode the rendered file's audio in the browser ----
	const MAX_DECODE_BYTES = 160 * 1024 * 1024;
	let decodedFor = '';
	$effect(() => {
		const path = s.outputPath;
		if (!path || path === decodedFor) return;
		decodedFor = path;
		peaks = null;
		void (async () => {
			try {
				const url = outputFileUrl(path);
				const head = await fetch(url, { method: 'HEAD' });
				const size = Number(head.headers.get('content-length') ?? 0);
				if (size > MAX_DECODE_BYTES) return; // too big to decode just for a waveform
				const buf = await (await fetch(url)).arrayBuffer();
				const ctx = new OfflineAudioContext(1, 2, 22050);
				const audio = await ctx.decodeAudioData(buf);
				if (decodedFor !== path) return;
				const ch = audio.getChannelData(0);
				const n = 1600;
				const out = new Float32Array(n);
				const step = Math.max(1, Math.floor(ch.length / n));
				let max = 0;
				for (let i = 0; i < n; i++) {
					let m = 0;
					const end = Math.min(ch.length, (i + 1) * step);
					for (let j = i * step; j < end; j += 4) m = Math.max(m, Math.abs(ch[j]));
					out[i] = m;
					max = Math.max(max, m);
				}
				if (max > 0) for (let i = 0; i < n; i++) out[i] /= max;
				peaks = out;
			} catch {
				peaks = null; // the timeline still works without a waveform
			}
		})();
	});

	// ---- per-segment thumbnails from the rendered video ----
	let thumbsFor = '';
	$effect(() => {
		const path = s.outputPath;
		const segs = layout;
		// Keyed on the segment boundaries, so another version with the
		// same number of moments still gets its own thumbnails.
		const key = `${path}|${segs.map((g) => g.startSecs.toFixed(2)).join(',')}`;
		if (!path || segs.length === 0 || thumbsFor === key) return;
		thumbsFor = key;
		thumbs = {};
		void (async () => {
			const mids = segs.map((g) => (g.startSecs + g.endSecs) / 2);
			for (let i = 0; i < mids.length; i += 16) {
				const chunk = mids.slice(i, i + 16);
				try {
					const r = await inspectAt(path, chunk);
					if (thumbsFor !== key) return;
					const next = { ...thumbs };
					r.frames_inline.forEach((f, k) => (next[i + k] = f.data_uri));
					thumbs = next;
				} catch {
					/* cosmetic */
				}
			}
		})();
	});

	// ---- draw waveform + beat grid ----
	$effect(() => {
		if (!canvas || width <= 0) return;
		const dpr = window.devicePixelRatio || 1;
		const h = 64;
		canvas.width = Math.round(width * dpr);
		canvas.height = Math.round(h * dpr);
		const g = canvas.getContext('2d');
		if (!g) return;
		g.setTransform(dpr, 0, 0, dpr, 0, 0);
		g.clearRect(0, 0, width, h);

		// Beats: thin ticks, every 4th a bar line.
		const beats = s.beatTimes;
		beats.forEach((t, i) => {
			const x = (t / duration) * width;
			const bar = i % 4 === 0;
			g.fillStyle = bar ? 'rgba(255,255,255,0.22)' : 'rgba(255,255,255,0.08)';
			g.fillRect(Math.round(x), bar ? 0 : 10, 1, bar ? h : h - 20);
		});

		// Waveform, mirrored, with the brand gradient.
		if (peaks) {
			const grad = g.createLinearGradient(0, 0, width, 0);
			grad.addColorStop(0, '#ff7a45');
			grad.addColorStop(0.52, '#ff3d81');
			grad.addColorStop(1, '#7b5cff');
			g.fillStyle = grad;
			const n = peaks.length;
			const mid = h / 2;
			const cols = Math.min(n, Math.floor(width / 2));
			for (let c = 0; c < cols; c++) {
				const i0 = Math.floor((c / cols) * n);
				const i1 = Math.floor(((c + 1) / cols) * n);
				let m = 0;
				for (let i = i0; i < i1; i++) m = Math.max(m, peaks[i]);
				const amp = Math.max(1, m * (mid - 4));
				g.globalAlpha = 0.85;
				g.fillRect(c * 2, mid - amp, 1.2, amp * 2);
			}
			g.globalAlpha = 1;
		}
	});

	function seekFromEvent(e: MouseEvent) {
		if (!wrap || duration <= 0) return;
		const r = wrap.getBoundingClientRect();
		const t = ((e.clientX - r.left) / r.width) * duration;
		onSeek(Math.min(duration, Math.max(0, t)));
	}

	function moveHover(e: MouseEvent) {
		if (!wrap || duration <= 0) return;
		const r = wrap.getBoundingClientRect();
		hoverT = ((e.clientX - r.left) / r.width) * duration;
	}

	function selectSeg(i: number) {
		s.selectedSegmentIndex = i;
		onSeek(layout[i].startSecs + 0.05);
	}
</script>

<div class="timeline">
	<div class="head">
		<span class="eyebrow">Timeline</span>
		<span class="legend">
			<span><i class="k beat"></i>beats</span>
			<span><i class="k on-beat"></i>cut on the beat</span>
			{#if !s.exactTimeline}<span class="approx">approximate positions</span>{/if}
		</span>
		<span class="mono time">{clock(currentTime, true)} / {clock(duration)}</span>
	</div>

	<div
		class="track"
		role="presentation"
		bind:this={wrap}
		bind:clientWidth={width}
		onmousemove={moveHover}
		onmouseleave={() => (hoverT = null)}
	>
		<div
			class="wave"
			role="slider"
			tabindex="0"
			aria-label="Seek"
			aria-valuemin="0"
			aria-valuemax={Math.round(duration)}
			aria-valuenow={Math.round(currentTime)}
			aria-valuetext={clock(currentTime)}
			onclick={seekFromEvent}
			onkeydown={(e) => {
				const step = e.shiftKey ? 5 : 1;
				if (e.key === 'ArrowRight') onSeek(Math.min(duration, currentTime + step));
				else if (e.key === 'ArrowLeft') onSeek(Math.max(0, currentTime - step));
				else if (e.key === 'Home') onSeek(0);
				else if (e.key === 'End') onSeek(duration);
				else return;
				e.preventDefault();
			}}
		>
			<canvas bind:this={canvas} style="width: 100%; height: 64px" aria-hidden="true"></canvas>
			<!-- The "locked sweep": when a new version loads, a scan line runs
			     left to right and each cut snaps into place as it passes. -->
			{#key s.outputPath}
				<span class="sweep" aria-hidden="true"></span>
				{#each cuts as c, i (i)}
					<span
						class="cut"
						class:on={Math.abs(c.error_frames) <= tolerance}
						style="left: {pctOf(c.output_secs)}%; --delay: {Math.round((pctOf(c.output_secs) / 100) * 1100)}ms"
						title="Cut at {clock(c.output_secs, true)} — {Math.abs(c.error_ms).toFixed(0)} ms from the beat"
					></span>
				{/each}
			{/key}
		</div>

		<!-- One tab stop; arrows / Home / End pick a moment (radio group). -->
		<div class="segs" role="radiogroup" aria-label="Moments in your video" use:radiogroup>
			{#if introEnd > 0.2}
				<span class="card-block" style="left: 0; width: {pctOf(introEnd)}%" title="Opening title card" aria-hidden="true"
					>Title</span
				>
			{/if}
			{#if outroStart !== null && duration - outroStart > 0.2}
				<span
					class="card-block"
					style="left: {pctOf(outroStart)}%; width: {pctOf(duration - outroStart)}%"
					title="End card"
					aria-hidden="true">End</span
				>
			{/if}
			{#if hero}
				<span class="hero-pin" style="left: {pctOf((hero.startSecs + hero.endSecs) / 2)}%" aria-hidden="true"
					>★ Biggest moment</span
				>
			{/if}
			{#each layout as g (g.index)}
				{@const color = sourceColor(g.segment.source_id.replace(/^burst:/, ''), sourceIds)}
				{@const burst = g.segment.source_id.startsWith('burst:')}
				<button
					type="button"
					class="seg"
					class:sel={s.selectedSegmentIndex === g.index}
					class:burst
					class:playing={currentTime >= g.startSecs && currentTime < g.endSecs}
					style="left: {pctOf(g.startSecs)}%; width: {pctOf(g.endSecs - g.startSecs)}%; --c: {color}"
					aria-label="{burst ? 'Beat burst' : `Moment ${g.index + 1}`}, {clock(g.startSecs)} to {clock(g.endSecs)}"
					role="radio"
					aria-checked={s.selectedSegmentIndex === g.index}
					onclick={() => selectSeg(g.index)}
				>
					{#if thumbs[g.index]}<img src={thumbs[g.index]} alt="" />{/if}
					{#if burst}<span class="burst-tag">burst</span>{/if}
				</button>
			{/each}
		</div>

		<span class="playhead" style="left: {pctOf(currentTime)}%" aria-hidden="true"></span>
		{#if hoverT !== null}
			<span class="hover" style="left: {pctOf(hoverT)}%" aria-hidden="true">{clock(hoverT, true)}</span>
		{/if}
	</div>
</div>

<style>
	.timeline {
		display: flex;
		flex-direction: column;
		gap: 8px;
	}

	.head {
		display: flex;
		align-items: center;
		gap: 14px;
		flex-wrap: wrap;
	}

	.legend {
		display: flex;
		gap: 12px;
		font-size: var(--fs-xs);
		color: var(--text-faint);
		flex: 1;
	}

	.legend span {
		display: inline-flex;
		align-items: center;
		gap: 5px;
	}

	.k {
		display: inline-block;
		width: 8px;
		height: 8px;
	}

	.k.beat {
		width: 2px;
		height: 10px;
		background: rgba(255, 255, 255, 0.45);
	}

	.k.on-beat {
		transform: rotate(45deg);
		background: var(--good);
		border-radius: 2px;
	}

	.approx {
		color: var(--warn);
	}

	.time {
		font-size: var(--fs-sm);
		color: var(--text-dim);
		font-variant-numeric: tabular-nums;
	}

	.track {
		position: relative;
		border-radius: var(--r-md);
		background: rgba(0, 0, 0, 0.35);
		border: 1px solid var(--line);
		padding: 10px 0 10px;
		user-select: none;
	}

	.wave {
		position: relative;
		height: 64px;
		cursor: pointer;
	}

	.cut {
		position: absolute;
		top: -6px;
		width: 9px;
		height: 9px;
		margin-left: -4.5px;
		transform: rotate(45deg);
		border-radius: 2px;
		background: var(--text-faint);
		box-shadow: 0 0 0 2px rgba(7, 8, 12, 0.9);
		pointer-events: auto;
	}

	.cut.on {
		background: var(--good);
		box-shadow:
			0 0 0 2px rgba(7, 8, 12, 0.9),
			0 0 12px rgba(74, 222, 128, 0.8);
	}

	.cut {
		animation: lock 520ms var(--delay, 0ms) var(--ease-spring) both;
	}

	.sweep {
		position: absolute;
		top: -10px;
		bottom: -4px;
		left: 0;
		width: 2px;
		background: linear-gradient(to bottom, transparent, var(--good), transparent);
		box-shadow: 0 0 18px 4px rgba(74, 222, 128, 0.45);
		pointer-events: none;
		animation: sweep 1100ms var(--ease-out) both;
	}

	@keyframes lock {
		0% {
			opacity: 0;
			transform: translateY(-14px) rotate(45deg) scale(0.4);
		}
		60% {
			opacity: 1;
			transform: translateY(2px) rotate(45deg) scale(1.35);
		}
		100% {
			transform: rotate(45deg) scale(1);
		}
	}

	@keyframes sweep {
		from {
			left: 0;
			opacity: 1;
		}
		90% {
			opacity: 1;
		}
		to {
			left: 100%;
			opacity: 0;
		}
	}

	@media (prefers-reduced-motion: reduce) {
		.cut,
		.sweep {
			animation: none;
		}
		.sweep {
			display: none;
		}
	}

	.segs {
		position: relative;
		height: 58px;
		margin-top: 8px;
	}

	.seg {
		position: absolute;
		top: 0;
		bottom: 0;
		padding: 0;
		border: none;
		border-radius: 7px;
		overflow: hidden;
		background: color-mix(in srgb, var(--c) 35%, #0d0f16);
		box-shadow:
			inset 0 0 0 1px rgba(0, 0, 0, 0.5),
			inset 0 -3px 0 var(--c);
		cursor: pointer;
		transition:
			transform var(--dur-1) var(--ease-out),
			box-shadow var(--dur-1);
	}

	.seg img {
		width: 100%;
		height: 100%;
		object-fit: cover;
		opacity: 0.85;
		animation: fade var(--dur-2) var(--ease-out) both;
	}

	.seg:hover {
		transform: translateY(-3px);
		z-index: 2;
	}

	.seg.playing {
		box-shadow:
			inset 0 0 0 2px rgba(255, 255, 255, 0.85),
			inset 0 -3px 0 var(--c);
	}

	/* Keep the focus ring above the neighbouring blocks. */
	.seg:focus-visible {
		z-index: 4;
	}

	.seg.sel {
		z-index: 3;
		transform: translateY(-4px);
		box-shadow:
			0 0 0 2px var(--magenta),
			0 10px 24px -6px rgba(255, 61, 129, 0.8);
	}

	.seg.burst {
		background: repeating-linear-gradient(
			90deg,
			color-mix(in srgb, var(--c) 40%, #0d0f16) 0 6px,
			#0d0f16 6px 7px
		);
	}

	@media (max-width: 640px) {
		.legend {
			visibility: hidden;
		}
	}

	.hero-pin {
		position: absolute;
		top: -21px;
		transform: translateX(-50%);
		padding: 1px 8px;
		border-radius: 999px;
		font-size: 10.5px;
		font-weight: 800;
		letter-spacing: 0.04em;
		white-space: nowrap;
		color: #160a12;
		background: var(--magenta);
		box-shadow: 0 0 14px rgba(255, 61, 129, 0.8);
		z-index: 4;
		pointer-events: none;
	}

	.card-block {
		position: absolute;
		top: 0;
		bottom: 0;
		display: grid;
		place-items: center;
		border-radius: 7px;
		border: 1px dashed var(--line-strong);
		background: repeating-linear-gradient(
			135deg,
			rgba(255, 255, 255, 0.03) 0 6px,
			rgba(255, 255, 255, 0.07) 6px 12px
		);
		font-family: var(--serif);
		font-style: italic;
		font-size: 15px;
		color: var(--text-dim);
		overflow: hidden;
		pointer-events: none;
	}

	.burst-tag {
		position: absolute;
		left: 4px;
		top: 4px;
		font-size: 10px;
		font-weight: 800;
		letter-spacing: 0.08em;
		text-transform: uppercase;
		padding: 1px 5px;
		border-radius: 4px;
		background: var(--magenta);
		color: #160a12;
	}

	.playhead {
		position: absolute;
		top: 0;
		bottom: 0;
		width: 2px;
		margin-left: -1px;
		background: #fff;
		box-shadow: 0 0 14px rgba(255, 255, 255, 0.9);
		pointer-events: none;
		z-index: 5;
	}

	.playhead::before {
		content: '';
		position: absolute;
		top: -5px;
		left: -5px;
		width: 12px;
		height: 12px;
		border-radius: 50%;
		background: #fff;
	}

	.hover {
		position: absolute;
		top: -26px;
		transform: translateX(-50%);
		font-family: var(--mono);
		font-size: 11px;
		padding: 2px 6px;
		border-radius: 6px;
		background: var(--panel-hi);
		border: 1px solid var(--line-strong);
		pointer-events: none;
		z-index: 6;
	}

	@keyframes fade {
		from {
			opacity: 0;
		}
	}
</style>
