<script lang="ts">
	import { appState as s } from '$lib/state.svelte';
	import { ApiError, inspectAt, outputFileUrl, reformat, reformatStatus } from '$lib/api';
	import type { PlatformPreset, ReformatResult } from '$lib/api';
	import { clock } from '$lib/vibes';
	import { radiogroup } from '$lib/radiogroup';
	import { cropBoxAt, fitFor, previewTime, progressLabel, reframes } from '$lib/reframe';
	import type { Framing } from '$lib/reframe';

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
	// (POST /api/reformat), reframed to the platform's shape. A different
	// shape is framed one of two ways: a crop that follows the action
	// (the default), or the whole frame over a blurred fill of itself.
	type Platform = { preset: PlatformPreset; label: string; shape: string; w: number; h: number };
	const PLATFORMS: Platform[] = [
		{ preset: 'youtube', label: 'YouTube', shape: '16:9', w: 16, h: 9 },
		{ preset: 'reels', label: 'Reels / TikTok / Shorts', shape: '9:16', w: 9, h: 16 },
		{ preset: 'square', label: 'Square', shape: '1:1', w: 1, h: 1 },
		{ preset: 'portrait', label: 'Portrait', shape: '4:5', w: 4, h: 5 },
		{ preset: 'whatsapp', label: 'WhatsApp', shape: '9:16 · small', w: 9, h: 16 }
	];
	type Version =
		| { state: 'working'; progress: number }
		| { state: 'done'; result: ReformatResult }
		| { state: 'failed'; error: string };
	/** Keyed by render + preset + framing, so a new render starts with no
	 *  versions and each framing is its own file. */
	let versions = $state<Record<string, Version>>({});
	let chosen = $state<PlatformPreset | null>(null);
	let framing = $state<Framing>('smart');
	const versionKey = (source: string, preset: PlatformPreset, fit: Framing | undefined) =>
		`${source}|${preset}|${fit ?? ''}`;
	const chosenPlatform = $derived(PLATFORMS.find((p) => p.preset === chosen) ?? null);
	/** The platform changes the render's shape, so its framing matters. */
	const reshapes = (p: Platform) => reframes(s.manifest?.resolution, p.w, p.h);
	const fitOf = (p: Platform) => fitFor(framing, reshapes(p));
	const chosenFit = $derived(chosenPlatform ? fitOf(chosenPlatform) : undefined);
	const currentKey = $derived(chosen && s.outputPath ? versionKey(s.outputPath, chosen, chosenFit) : null);
	const current = $derived(currentKey ? versions[currentKey] : undefined);
	/** The framing choice is offered until a same-shape platform is picked. */
	const showFraming = $derived(!chosenPlatform || reshapes(chosenPlatform));
	const versionFileName = $derived(
		current?.state === 'done' ? (current.result.output_path.split('/').pop() ?? fileName) : fileName
	);

	function setFraming(f: Framing) {
		framing = f;
		if (chosen) makeVersion(chosen);
	}

	async function makeVersion(preset: PlatformPreset) {
		const source = s.outputPath;
		const platform = PLATFORMS.find((p) => p.preset === preset);
		if (!source || !platform) return;
		chosen = preset;
		const fit = fitOf(platform);
		const key = versionKey(source, preset, fit);
		const existing = versions[key];
		if (existing && existing.state !== 'failed') return;
		versions[key] = { state: 'working', progress: 0 };
		try {
			// A version already being made (another tab, a reopened sheet)
			// comes back as that same job (`reused`): this just follows it.
			const job = await reformat(source, preset, fit);
			for (;;) {
				const st = await reformatStatus(job.job_id);
				if (st.state === 'done') {
					versions[key] = { state: 'done', result: st };
					if (fit) loadPreview(key, source, st);
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

	// Before / after: the render and the version at one moment (two frames
	// from GET /api/frames/at), with the window a smart crop kept marked
	// on the render — at the moment it sat furthest from the centre.
	type Preview = {
		t: number;
		box: { left: number; top: number; width: number; height: number } | null;
		before?: string;
		after?: string;
		failed?: boolean;
	};
	let previews = $state<Record<string, Preview>>({});
	const preview = $derived(current?.state === 'done' && currentKey ? previews[currentKey] : undefined);
	const sourceAspect = $derived((s.manifest?.resolution ?? '16x9').replace(/[x×]/, ' / '));

	async function loadPreview(key: string, source: string, r: ReformatResult) {
		const t = previewTime(r.smart, r.duration_secs);
		previews[key] = { t, box: r.smart ? cropBoxAt(r.smart, t) : null };
		try {
			const [before, after] = await Promise.all([inspectAt(source, [t]), inspectAt(r.output_path, [t])]);
			previews[key] = {
				...previews[key],
				before: before.frames_inline[0]?.data_uri,
				after: after.frames_inline[0]?.data_uri
			};
		} catch {
			// Only a nicety: the version itself is ready either way.
			previews[key] = { ...previews[key], failed: true };
		}
	}

	const versionNote = $derived.by(() => {
		if (!chosenPlatform || !current) return '';
		const label = chosenPlatform.label;
		if (current.state === 'working') {
			return chosenFit === 'smart' ? `Making the ${label} version, following the action…` : `Making the ${label} version…`;
		}
		if (current.state === 'done') {
			const r = current.result;
			const how = r.fit === 'smart' ? ', following the action' : r.fit === 'blur' ? ', over a blurred fill' : '';
			return `${label} version ready${how}: ${r.width} by ${r.height}, ${megabytes(r.size_bytes)}.`;
		}
		return `Couldn’t make the ${label} version: ${current.error}`;
	});

	function megabytes(bytes: number): string {
		return `${(bytes / 1_000_000).toFixed(bytes < 10_000_000 ? 1 : 0)} MB`;
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
		{#if showFraming}
			<!-- How a vertical, square or 4:5 version fills its new shape. -->
			<div class="framing">
				<span class="f-label" id="framing-h">Framing</span>
				<div class="seg" role="radiogroup" aria-labelledby="framing-h" use:radiogroup={{ selectOnMove: false }}>
					<button
						type="button"
						role="radio"
						aria-checked={framing === 'smart'}
						aria-describedby="framing-hint"
						onclick={() => setFraming('smart')}
					>
						<svg width="14" height="14" viewBox="0 0 24 24" aria-hidden="true"
							><path
								d="M4 8V5a1 1 0 0 1 1-1h3M16 4h3a1 1 0 0 1 1 1v3M20 16v3a1 1 0 0 1-1 1h-3M8 20H5a1 1 0 0 1-1-1v-3"
								fill="none"
								stroke="currentColor"
								stroke-width="2"
								stroke-linecap="round"
							/><circle cx="13.5" cy="12" r="3" fill="currentColor" /></svg
						>
						Follow the action
					</button>
					<button
						type="button"
						role="radio"
						aria-checked={framing === 'blur'}
						aria-describedby="framing-hint"
						onclick={() => setFraming('blur')}
					>
						<svg width="14" height="14" viewBox="0 0 24 24" aria-hidden="true"
							><rect x="3" y="4" width="18" height="16" rx="2" fill="currentColor" opacity="0.3" /><rect
								x="7"
								y="8"
								width="10"
								height="8"
								rx="1"
								fill="currentColor"
							/></svg
						>
						Blur fill
					</button>
				</div>
				<span class="f-hint" id="framing-hint"
					>{framing === 'smart'
						? 'Crops in and moves with whatever is moving.'
						: 'Keeps the whole frame, over a soft fill of itself.'}</span
				>
			</div>
		{/if}
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
					<p class="v-note">{progressLabel(chosenFit, current.progress, chosenPlatform.label)}</p>
				{:else if current.state === 'done'}
					{@const r = current.result}
					<p class="v-note">
						<strong>{chosenPlatform.label}</strong>{' version ready '}<span class="mono"
							>· {r.width}×{r.height} · {clock(r.duration_secs)} · {megabytes(r.size_bytes)}</span
						>
					</p>
					{#if preview && !preview.failed}
						{@const box = preview.box}
						<figure class="compare">
							<div class="shot" style:aspect-ratio={sourceAspect}>
								{#if preview.before}
									<img src={preview.before} alt="Your video at {clock(preview.t)}" />
								{/if}
								{#if box}
									<span
										class="window"
										style:left="{box.left}%"
										style:top="{box.top}%"
										style:width="{box.width}%"
										style:height="{box.height}%"
									></span>
								{/if}
								<span class="tag">Before</span>
							</div>
							<svg class="arrow" width="18" height="18" viewBox="0 0 24 24" aria-hidden="true"
								><path
									d="M5 12h13m0 0-5-5m5 5-5 5"
									fill="none"
									stroke="currentColor"
									stroke-width="2"
									stroke-linecap="round"
									stroke-linejoin="round"
								/></svg
							>
							<div class="shot" style:aspect-ratio="{r.width} / {r.height}">
								{#if preview.after}
									<img src={preview.after} alt="The {chosenPlatform.label} version at {clock(preview.t)}" />
								{/if}
								<span class="tag">After</span>
							</div>
							<figcaption>
								{#if r.fit === 'smart' && r.smart && r.smart.followed_shots === 0}
									At {clock(preview.t)}: nothing moved clearly enough to follow, so it keeps the middle of the frame.
								{:else if r.fit === 'smart' && box && r.smart}
									At {clock(preview.t)}: the lit window is what the {chosenPlatform.label} version keeps. It follows
									the action in {r.smart.followed_shots} of {r.smart.shots}
									{r.smart.shots === 1 ? 'shot' : 'shots'}.
								{:else if r.fit === 'smart'}
									At {clock(preview.t)}, cropped to follow the action.
								{:else}
									At {clock(preview.t)}: the whole frame, over a blurred fill of itself.
								{/if}
							</figcaption>
						</figure>
					{/if}
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

	.formats {
		margin: 14px 22px 0;
	}

	.platforms {
		display: grid;
		grid-template-columns: repeat(5, minmax(0, 1fr));
		gap: 8px;
		margin-top: 8px;
	}

	.framing {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 6px 10px;
		margin-top: 10px;
	}

	.f-label {
		font-size: var(--fs-xs);
		font-weight: 600;
		color: var(--text-faint);
	}

	.seg {
		display: inline-flex;
		padding: 3px;
		gap: 2px;
		border-radius: 999px;
		border: 1px solid var(--line);
		background: rgba(255, 255, 255, 0.03);
	}

	.seg button {
		display: inline-flex;
		align-items: center;
		gap: 6px;
		padding: 5px 12px;
		border: 0;
		border-radius: 999px;
		background: transparent;
		color: var(--text-dim);
		font: inherit;
		font-size: var(--fs-xs);
		font-weight: 600;
		cursor: pointer;
		transition:
			background var(--dur-1) var(--ease-out),
			color var(--dur-1) var(--ease-out);
	}

	.seg button:hover {
		color: var(--text);
	}

	.seg button[aria-checked='true'] {
		color: var(--ink);
		background: var(--text);
	}

	.f-hint {
		flex-basis: 100%;
		font-size: var(--fs-xs);
		color: var(--text-faint);
	}

	.compare {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 8px 10px;
		margin: 12px 0 0;
	}

	.shot {
		position: relative;
		flex: none;
		height: 120px;
		border-radius: var(--r-sm);
		border: 1px solid var(--line);
		background: rgba(255, 255, 255, 0.04);
		overflow: hidden;
	}

	.shot:not(:has(img)) {
		animation: pulse 1.4s var(--ease-in-out) infinite alternate;
	}

	.shot img {
		display: block;
		width: 100%;
		height: 100%;
		object-fit: cover;
	}

	/* The window the smart crop keeps; the rest of the frame is dimmed. */
	.window {
		position: absolute;
		border: 2px solid var(--accent);
		border-radius: 3px;
		box-shadow: 0 0 0 999px rgba(7, 8, 12, 0.62);
		pointer-events: none;
	}

	.tag {
		position: absolute;
		left: 6px;
		bottom: 6px;
		padding: 1px 7px;
		border-radius: 999px;
		background: rgba(7, 8, 12, 0.72);
		color: var(--text);
		font-size: 11px;
		font-weight: 600;
		letter-spacing: 0.02em;
	}

	.arrow {
		color: var(--text-faint);
	}

	.compare figcaption {
		flex-basis: 100%;
		font-size: var(--fs-xs);
		color: var(--text-faint);
	}

	@keyframes pulse {
		from {
			opacity: 0.5;
		}
		to {
			opacity: 1;
		}
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

	@media (max-width: 480px) {
		.platforms {
			grid-template-columns: repeat(2, minmax(0, 1fr));
		}

		/* Five platforms in two columns: the last one takes the full row. */
		.platform:last-child:nth-child(odd) {
			grid-column: 1 / -1;
		}

		.shot {
			height: 96px;
		}
	}

	@media (prefers-reduced-motion: reduce) {
		.shot:not(:has(img)) {
			animation: none;
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
