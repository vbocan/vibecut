<script lang="ts">
	import { appState as s } from '$lib/state.svelte';
	import { ApiError, exportTimeline, outputFileUrl, reformat, reformatStatus } from '$lib/api';
	import type { PlatformPreset, ReformatResult, TimelineFormat } from '$lib/api';
	import { saveBlob } from '$lib/download';
	import { clock } from '$lib/vibes';
	import { radiogroup } from '$lib/radiogroup';

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
			// CC BY and friends: the credit the licence asks for, from the
			// shared version's own render record when it has one.
			...s.captionMusic,
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

	async function shareFile(fileUrl: string, name: string) {
		sharing = true;
		shareError = null;
		try {
			const blob = await (await fetch(fileUrl)).blob();
			const file = new File([blob], name, { type: 'video/mp4' });
			await navigator.share({ files: [file], title, text: caption });
		} catch (e) {
			if (!(e instanceof DOMException && e.name === 'AbortError')) {
				shareError = 'Sharing didn’t work here — download the file instead.';
			}
		} finally {
			sharing = false;
		}
	}

	// Platform versions: one fast server-side pass over this render
	// (POST /api/reformat), reframed to the platform's shape — a blurred
	// fill of the same frame behind it when the aspect differs.
	const PLATFORMS: { preset: PlatformPreset; label: string; shape: string; w: number; h: number }[] = [
		{ preset: 'youtube', label: 'YouTube', shape: '16:9', w: 16, h: 9 },
		{ preset: 'reels', label: 'Reels / TikTok / Shorts', shape: '9:16', w: 9, h: 16 },
		{ preset: 'square', label: 'Square', shape: '1:1', w: 1, h: 1 },
		{ preset: 'whatsapp', label: 'WhatsApp', shape: '9:16 · small', w: 9, h: 16 }
	];
	type Version =
		| { state: 'working'; progress: number }
		| { state: 'done'; result: ReformatResult }
		| { state: 'failed'; error: string };
	/** Keyed by render + preset, so a new render starts with no versions. */
	let versions = $state<Record<string, Version>>({});
	let chosen = $state<PlatformPreset | null>(null);
	const versionKey = (source: string, preset: PlatformPreset) => `${source}|${preset}`;
	const chosenPlatform = $derived(PLATFORMS.find((p) => p.preset === chosen) ?? null);
	const current = $derived(chosen && s.outputPath ? versions[versionKey(s.outputPath, chosen)] : undefined);
	const versionFileName = $derived(chosen ? `${fileName.replace(/\.mp4$/i, '')}.${chosen}.mp4` : fileName);

	async function makeVersion(preset: PlatformPreset) {
		const source = s.outputPath;
		if (!source) return;
		chosen = preset;
		const key = versionKey(source, preset);
		const existing = versions[key];
		if (existing && existing.state !== 'failed') return;
		versions[key] = { state: 'working', progress: 0 };
		try {
			// A version already being made (another tab, a reopened sheet)
			// comes back as that same job (`reused`): this just follows it.
			const job = await reformat(source, preset);
			for (;;) {
				const st = await reformatStatus(job.job_id);
				if (st.state === 'done') {
					versions[key] = { state: 'done', result: st };
					return;
				}
				if (st.state === 'failed') {
					versions[key] = { state: 'failed', error: st.error };
					return;
				}
				if (st.state === 'cancelled') {
					// Stopped from the Renders list; the last good version stays.
					versions[key] = { state: 'failed', error: 'it was stopped' };
					return;
				}
				if (st.state === 'running' && st.progress) {
					versions[key] = { state: 'working', progress: st.progress.overall };
				}
				await new Promise((r) => setTimeout(r, 400));
			}
		} catch (e) {
			versions[key] = { state: 'failed', error: e instanceof ApiError ? e.describe() : String(e) };
		}
	}

	const versionNote = $derived.by(() => {
		if (!chosenPlatform || !current) return '';
		const label = chosenPlatform.label;
		if (current.state === 'working') return `Making the ${label} version…`;
		if (current.state === 'done') {
			const r = current.result;
			return `${label} version ready: ${r.width} by ${r.height}, ${megabytes(r.size_bytes)}.`;
		}
		return `Couldn’t make the ${label} version: ${current.error}`;
	});

	function megabytes(bytes: number): string {
		return `${(bytes / 1_000_000).toFixed(bytes < 10_000_000 ? 1 : 0)} MB`;
	}

	// Open in an editor: this version's cut as a timeline of the original
	// clips (GET /api/export) — dissolves, ramps, title cards, the music
	// and its beats — to finish in Final Cut, Resolve or an OTIO editor.
	const EDITORS: { format: TimelineFormat; label: string; tag: string }[] = [
		{ format: 'fcpxml', label: 'Final Cut / Resolve', tag: 'FCPXML' },
		{ format: 'otio', label: 'OpenTimelineIO', tag: '.otio' }
	];
	let exporting = $state<TimelineFormat | null>(null);
	let exportError = $state<string | null>(null);
	let exported = $state<string | null>(null);
	/** Renders from before the exact timeline was recorded can't export. */
	const timelineMissing = $derived(!!s.manifest && !s.manifest.timeline);

	async function saveTimeline(format: TimelineFormat) {
		const path = s.outputPath;
		if (!path || exporting) return;
		exporting = format;
		exportError = null;
		exported = null;
		try {
			// Fetched first, so a failure reads as a message here rather
			// than as a broken download.
			const { blob, fileName } = await exportTimeline(path, format);
			saveBlob(blob, fileName);
			exported = fileName;
		} catch (e) {
			exportError = e instanceof ApiError ? e.describe() : String(e);
		} finally {
			exporting = null;
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
			<button class="btn big" type="button" disabled={sharing} onclick={() => shareFile(url, fileName)}>
				{sharing ? 'Preparing…' : 'Share…'}
			</button>
		{/if}
	</div>
	{#if shareError}<p class="error" role="alert">{shareError}</p>{/if}

	<section class="formats" aria-labelledby="formats-h">
		<span class="eyebrow" id="formats-h">Make a version for</span>
		<!-- One platform at a time: a radio group (arrows move, Space /
		     Enter picks — picking starts a version, so arrows alone don't). -->
		<div class="platforms" role="radiogroup" aria-labelledby="formats-h" use:radiogroup={{ selectOnMove: false }}>
			{#each PLATFORMS as p (p.preset)}
				<button
					type="button"
					class="platform"
					role="radio"
					aria-checked={chosen === p.preset}
					disabled={!s.outputPath}
					onclick={() => makeVersion(p.preset)}
				>
					<span class="frame-icon" style:aspect-ratio="{p.w} / {p.h}" aria-hidden="true"></span>
					<span class="p-name">{p.label}</span>
					<span class="p-shape mono">{p.shape}</span>
				</button>
			{/each}
		</div>
		<!-- Always on the page, so the outcome is read; not every percent. -->
		<p class="sr-only" role="status">{versionNote}</p>
		{#if chosenPlatform && current}
			<div class="version">
				{#if current.state === 'working'}
					{@const pct = Math.round(current.progress * 100)}
					<div
						class="bar"
						role="progressbar"
						aria-label="Making the {chosenPlatform.label} version"
						aria-valuemin={0}
						aria-valuemax={100}
						aria-valuenow={pct}
					>
						<span style:width="{Math.max(pct, 3)}%"></span>
					</div>
					<p class="v-note">Making the {chosenPlatform.label} version… {pct}%</p>
				{:else if current.state === 'done'}
					{@const r = current.result}
					<p class="v-note">
						<strong>{chosenPlatform.label}</strong>{' version ready '}<span class="mono"
							>· {r.width}×{r.height} · {clock(r.duration_secs)} · {megabytes(r.size_bytes)}</span
						>
					</p>
					<div class="v-actions">
						<a class="btn primary" href={outputFileUrl(r.output_path)} download={versionFileName}
							>Save {chosenPlatform.label} version</a
						>
						{#if canShareFiles}
							<button
								class="btn"
								type="button"
								disabled={sharing}
								onclick={() => shareFile(outputFileUrl(r.output_path), versionFileName)}
							>
								{sharing ? 'Preparing…' : 'Share…'}
							</button>
						{/if}
					</div>
				{:else}
					<p class="error inline">Couldn’t make the {chosenPlatform.label} version: {current.error}</p>
					<button class="btn small" type="button" onclick={() => chosen && makeVersion(chosen)}>Try again</button>
				{/if}
			</div>
		{/if}
	</section>

	<section class="editors" aria-labelledby="editors-h">
		<span class="eyebrow" id="editors-h">Open in an editor</span>
		<p class="e-note">
			The cut as a timeline of your original clips — every dissolve, speed ramp, title and beat in place — to
			finish in Final Cut Pro, DaVinci Resolve or any editor that reads OpenTimelineIO.
		</p>
		<div class="e-actions">
			{#each EDITORS as ed (ed.format)}
				<button
					type="button"
					class="btn editor"
					disabled={!s.outputPath || timelineMissing || exporting !== null}
					aria-busy={exporting === ed.format}
					onclick={() => saveTimeline(ed.format)}
				>
					<svg width="18" height="18" viewBox="0 0 24 24" aria-hidden="true"
						><path
							d="M3 6h9M3 12h18M3 18h6M15 3v6M12 15v6"
							fill="none"
							stroke="currentColor"
							stroke-width="2"
							stroke-linecap="round"
						/></svg
					>
					<span class="e-name">{exporting === ed.format ? 'Preparing…' : ed.label}</span>
					<span class="e-tag mono">{ed.tag}</span>
				</button>
			{/each}
		</div>
		<p class="sr-only" role="status">{exported ? `Saved ${exported}` : ''}</p>
		{#if timelineMissing}
			<p class="e-hint">This version predates timeline export — render it again to open it in an editor.</p>
		{:else if exportError}
			<p class="error inline" role="alert">Couldn’t export the timeline: {exportError}</p>
		{:else}
			<p class="e-hint">
				{#if exported}<span class="e-saved">Saved {exported}.</span>{' '}{/if}Clips link to your original footage; if
				the editor asks where it is, point it at your footage folder.
			</p>
		{/if}
	</section>

	<div class="caption">
		<div class="cap-head">
			<span class="eyebrow">Caption</span>
			<button class="btn ghost small" type="button" onclick={copyCaption}>{copied ? 'Copied ✓' : 'Copy'}</button>
			<span class="sr-only" role="status">{copied ? 'Caption copied' : ''}</span>
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
		/* Taller than a laptop window: scroll inside the sheet, not past it. */
		overflow-x: hidden;
		overflow-y: auto;
		overscroll-behavior: contain;
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

	.formats {
		margin: 14px 22px 0;
	}

	.platforms {
		display: grid;
		grid-template-columns: repeat(4, minmax(0, 1fr));
		gap: 8px;
		margin-top: 8px;
	}

	.platform {
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: 4px;
		padding: 10px 6px;
		border-radius: var(--r-md);
		border: 1px solid var(--line);
		background: rgba(255, 255, 255, 0.03);
		color: var(--text);
		font: inherit;
		font-size: var(--fs-xs);
		line-height: 1.2;
		text-align: center;
		cursor: pointer;
	}

	.platform:hover:not(:disabled) {
		border-color: var(--line-strong);
		background: rgba(255, 255, 255, 0.07);
	}

	.platform[aria-checked='true'] {
		border-color: var(--accent);
		background: rgba(255, 122, 69, 0.1);
	}

	.platform:disabled {
		opacity: 0.45;
		cursor: not-allowed;
	}

	.frame-icon {
		height: 22px;
		max-width: 40px;
		border: 2px solid currentColor;
		border-radius: 4px;
		opacity: 0.8;
	}

	.p-name {
		font-weight: 600;
	}

	.p-shape {
		color: var(--text-faint);
	}

	.version {
		margin-top: 10px;
	}

	.bar {
		height: 6px;
		border-radius: 3px;
		background: rgba(255, 255, 255, 0.08);
		overflow: hidden;
	}

	.bar span {
		display: block;
		height: 100%;
		background: var(--grad-brand);
		transition: width var(--dur-2) var(--ease-out);
	}

	.v-note {
		margin: 6px 0 0;
		font-size: var(--fs-sm);
		color: var(--text-dim);
	}

	.v-actions {
		display: flex;
		flex-wrap: wrap;
		gap: 8px;
		margin-top: 8px;
	}

	.v-actions .btn {
		text-decoration: none;
	}

	.error.inline {
		margin: 0 0 6px;
	}

	.editors {
		margin: 16px 22px 0;
	}

	.e-note {
		margin: 6px 0 0;
		font-size: var(--fs-sm);
		line-height: 1.45;
		color: var(--text-dim);
	}

	.e-actions {
		display: grid;
		grid-template-columns: repeat(2, minmax(0, 1fr));
		gap: 8px;
		margin-top: 10px;
	}

	.editor {
		justify-content: flex-start;
		min-width: 0;
		padding: 10px 14px;
		border-radius: var(--r-md);
	}

	.e-name {
		flex: 1;
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		text-align: left;
	}

	.e-tag {
		font-size: var(--fs-xs);
		font-weight: 500;
		color: var(--text-faint);
	}

	.e-hint {
		margin: 8px 0 0;
		font-size: var(--fs-xs);
		line-height: 1.45;
		color: var(--text-faint);
	}

	.e-saved {
		color: var(--text-dim);
	}

	.editors .error.inline {
		margin: 8px 0 0;
	}

	@media (max-width: 480px) {
		.platforms {
			grid-template-columns: repeat(2, minmax(0, 1fr));
		}

		.e-actions {
			grid-template-columns: 1fr;
		}
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
