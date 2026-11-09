<script lang="ts">
	import { onDestroy } from 'svelte';
	import { appState as s } from '$lib/state.svelte';
	import { outputFileUrl } from '$lib/api';
	import { prefersReducedMotion } from '$lib/motion';

	// The rendered video with an "ambilight": a tiny canvas copy of the
	// current frame, blurred and scaled behind the player so the screen
	// glows with the footage's colours.
	let {
		currentTime = $bindable(0),
		video = $bindable()
	}: { currentTime?: number; video?: HTMLVideoElement } = $props();

	let glow: HTMLCanvasElement | undefined = $state();
	let raf = 0;
	let last = 0;

	const src = $derived(s.outputPath ? outputFileUrl(s.outputPath) : null);
	// A frame from the middle of the video as the poster (early frames are
	// often the black title card), so the player — and its glow — show the
	// footage before anyone presses play.
	const poster = $derived(s.frames[Math.floor(s.frames.length / 2)]?.data_uri);

	$effect(() => {
		const uri = poster;
		if (!uri || !glow) return;
		const img = new Image();
		img.onload = () => {
			if (!video || video.paused) glow?.getContext('2d')?.drawImage(img, 0, 0, glow.width, glow.height);
		};
		img.src = uri;
	});

	function paint() {
		// Before playback the glow shows the poster (see the effect above).
		if (!video || !glow || video.readyState < 2 || (video.paused && video.currentTime === 0)) return;
		const g = glow.getContext('2d');
		if (!g) return;
		try {
			g.drawImage(video, 0, 0, glow.width, glow.height);
		} catch {
			/* cross-origin or not ready: skip */
		}
	}

	function loop(ts: number) {
		if (video) currentTime = video.currentTime;
		if (ts - last > 120) {
			paint();
			last = ts;
		}
		raf = requestAnimationFrame(loop);
	}

	$effect(() => {
		raf = requestAnimationFrame(loop);
		return () => cancelAnimationFrame(raf);
	});

	onDestroy(() => cancelAnimationFrame(raf));
</script>

<div class="frame">
	<canvas class="glow" bind:this={glow} width="32" height="18" aria-hidden="true"></canvas>
	{#if src}
		<!-- svelte-ignore a11y_media_has_caption -- auto-generated highlight reel; no caption source exists -->
		<video
			bind:this={video}
			aria-label="Your video"
			{src}
			{poster}
			controls
			playsinline
			preload="metadata"
			onloadedmetadata={() => {
				s.videoDuration = video?.duration ?? null;
				setTimeout(paint, 300);
				// The reveal: the music that played during the cut hands over
				// to the finished reel (unless the viewer asked for less motion).
				if (s.autoplayReveal) {
					s.autoplayReveal = false;
					if (!prefersReducedMotion()) void video?.play().catch(() => {});
				}
			}}
			onseeked={paint}
		></video>
	{/if}
</div>

<style>
	.frame {
		position: relative;
		aspect-ratio: 16 / 9;
		border-radius: var(--r-lg);
		isolation: isolate;
	}

	.glow {
		position: absolute;
		inset: -6%;
		width: 112%;
		height: 112%;
		z-index: -1;
		filter: blur(48px) saturate(1.6);
		opacity: 0.7;
		border-radius: 40px;
		transition: opacity var(--dur-3);
	}

	video {
		width: 100%;
		height: 100%;
		display: block;
		border-radius: var(--r-lg);
		background: #000;
		box-shadow:
			0 0 0 1px var(--line-strong),
			var(--shadow-2);
	}
</style>
