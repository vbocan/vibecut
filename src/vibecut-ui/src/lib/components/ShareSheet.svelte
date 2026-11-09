<script lang="ts">
	import { appState as s } from '$lib/state.svelte';
	import { outputFileUrl } from '$lib/api';
	import { clock } from '$lib/vibes';

	// Decision 2: get the video out. Download, the phone's own share sheet
	// where the browser supports sharing files, and a ready-to-paste caption
	// that credits the music.
	let { open = $bindable(false), title, fileName }: { open?: boolean; title: string; fileName: string } = $props();
	let dialog: HTMLDialogElement | undefined = $state();
	let copied = $state(false);
	let sharing = $state(false);
	let shareError = $state<string | null>(null);

	$effect(() => {
		if (!dialog) return;
		if (open && !dialog.open) dialog.showModal();
		if (!open && dialog.open) dialog.close();
	});

	const url = $derived(s.outputPath ? outputFileUrl(s.outputPath) : '');
	const poster = $derived(s.frames[Math.floor(s.frames.length / 2)]?.data_uri);
	const seconds = $derived(s.videoDuration ?? s.renderResult?.manifest_summary?.actual_duration_secs ?? 0);
	const beat = $derived(s.manifest?.beat_sync ?? null);
	const canShareFiles = $derived(
		typeof navigator !== 'undefined' &&
			'canShare' in navigator &&
			navigator.canShare?.({ files: [new File([''], 'x.mp4', { type: 'video/mp4' })] }) === true
	);

	const caption = $derived(
		[
			title,
			s.effectiveTrack ? `Music: ${s.effectiveTrack.title}` : null,
			// Only claim the beat lock when the report backs it up.
			beat && beat.n_cuts > 0 && beat.within_tolerance === beat.n_cuts
				? 'Edited with Vibecut — every cut on the beat.'
				: 'Edited with Vibecut.'
		]
			.filter(Boolean)
			.join('\n')
	);

	async function copyCaption() {
		try {
			await navigator.clipboard.writeText(caption);
			copied = true;
			setTimeout(() => (copied = false), 2000);
		} catch {
			copied = false;
		}
	}

	async function shareFile() {
		sharing = true;
		shareError = null;
		try {
			const blob = await (await fetch(url)).blob();
			const file = new File([blob], fileName, { type: 'video/mp4' });
			await navigator.share({ files: [file], title, text: caption });
		} catch (e) {
			if (!(e instanceof DOMException && e.name === 'AbortError')) {
				shareError = 'Sharing didn’t work here — download the file instead.';
			}
		} finally {
			sharing = false;
		}
	}
</script>

<dialog bind:this={dialog} class="sheet" aria-labelledby="share-h" onclose={() => (open = false)}>
	<div class="poster">
		{#if poster}<img src={poster} alt="" />{/if}
		<div class="meta">
			<p class="eyebrow">Ready to share</p>
			<h2 id="share-h">{title}</h2>
			<p class="facts mono">
				{clock(seconds)} · {s.manifest?.resolution?.replace('x', '×') ?? ''} MP4{#if beat}&nbsp;· {beat.within_tolerance}/{beat.n_cuts} cuts on the beat{/if}
			</p>
		</div>
		<button class="close" type="button" aria-label="Close" onclick={() => (open = false)}>✕</button>
	</div>

	<div class="actions">
		<a class="btn primary big" href={url} download={fileName}>
			<svg width="20" height="20" viewBox="0 0 24 24" aria-hidden="true"
				><path
					d="M12 4v11m0 0 4.5-4.5M12 15l-4.5-4.5M5 19h14"
					fill="none"
					stroke="currentColor"
					stroke-width="2.2"
					stroke-linecap="round"
					stroke-linejoin="round"
				/></svg
			>
			Save video
		</a>
		{#if canShareFiles}
			<button class="btn big" type="button" disabled={sharing} onclick={shareFile}>
				{sharing ? 'Preparing…' : 'Share…'}
			</button>
		{/if}
	</div>
	{#if shareError}<p class="error">{shareError}</p>{/if}

	<div class="caption">
		<div class="cap-head">
			<span class="eyebrow">Caption</span>
			<button class="btn ghost small" type="button" onclick={copyCaption}>{copied ? 'Copied ✓' : 'Copy'}</button>
		</div>
		<pre>{caption}</pre>
	</div>

	<p class="privacy">
		<span aria-hidden="true">🔒</span> Your video stays on this computer. Nothing is uploaded anywhere.
	</p>
</dialog>

<style>
	.sheet {
		width: min(560px, calc(100vw - 24px));
		padding: 0;
		border-radius: var(--r-xl);
		border: 1px solid var(--line-strong);
		background: rgba(14, 16, 23, 0.97);
		color: var(--text);
		box-shadow: var(--shadow-2);
		overflow: hidden;
	}

	.sheet[open] {
		animation: pop var(--dur-3) var(--ease-spring) both;
	}

	.sheet::backdrop {
		background: rgba(3, 4, 7, 0.65);
		backdrop-filter: blur(8px);
	}

	.poster {
		position: relative;
		aspect-ratio: 16 / 8;
		background: #000;
	}

	.poster img {
		width: 100%;
		height: 100%;
		object-fit: cover;
		opacity: 0.85;
	}

	.poster::after {
		content: '';
		position: absolute;
		inset: 0;
		background: linear-gradient(to top, rgba(14, 16, 23, 1), rgba(14, 16, 23, 0.1) 70%);
	}

	.meta {
		position: absolute;
		left: 22px;
		right: 22px;
		bottom: 14px;
		z-index: 1;
	}

	.meta .eyebrow {
		margin: 0;
	}

	h2 {
		margin: 2px 0 2px;
		font-size: var(--fs-2xl);
		letter-spacing: -0.03em;
		line-height: 1.05;
	}

	.facts {
		margin: 0;
		font-size: var(--fs-xs);
		color: var(--text-dim);
	}

	.close {
		position: absolute;
		top: 12px;
		right: 12px;
		z-index: 2;
		width: 34px;
		height: 34px;
		border-radius: 50%;
		border: 1px solid var(--line-strong);
		background: rgba(7, 8, 12, 0.6);
		cursor: pointer;
	}

	.actions {
		display: flex;
		gap: 10px;
		padding: 18px 22px 6px;
	}

	.big {
		flex: 1;
		padding: 14px 18px;
		font-size: var(--fs-md);
		text-decoration: none;
	}

	.caption {
		margin: 12px 22px 0;
		padding: 12px 14px;
		border-radius: var(--r-md);
		border: 1px solid var(--line);
		background: rgba(255, 255, 255, 0.03);
	}

	.cap-head {
		display: flex;
		align-items: center;
		justify-content: space-between;
	}

	.small {
		padding: 4px 10px;
		font-size: var(--fs-xs);
	}

	pre {
		margin: 8px 0 0;
		white-space: pre-wrap;
		font-family: var(--ui);
		font-size: var(--fs-sm);
		color: var(--text-dim);
	}

	.privacy {
		margin: 14px 22px 20px;
		font-size: var(--fs-xs);
		color: var(--text-faint);
	}

	.error {
		margin: 6px 22px 0;
		color: var(--bad);
		font-size: var(--fs-sm);
	}

	@keyframes pop {
		from {
			opacity: 0;
			transform: translateY(24px) scale(0.97);
		}
	}
</style>
