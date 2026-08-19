<script lang="ts">
	import { appState as s } from '$lib/state.svelte';
	import { outputFileUrl } from '$lib/api';
	import { sourceColor } from '$lib/colors';
	import type { SegmentLayout } from '$lib/state.svelte';

	let videoEl: HTMLVideoElement | undefined = $state();
	let currentTime = $state(0);

	const activeIndex = $derived(s.segmentAt(currentTime)?.index ?? null);
	const totalSecs = $derived(s.segmentLayout.at(-1)?.endSecs ?? 0);
	const selected = $derived(
		s.selectedSegmentIndex !== null ? s.segmentLayout[s.selectedSegmentIndex] : null
	);

	function selectAndSeek(layout: SegmentLayout) {
		s.selectedSegmentIndex = layout.index;
		if (videoEl) videoEl.currentTime = layout.startSecs;
	}

	function fmtTime(t: number): string {
		const m = Math.floor(t / 60);
		const sec = (t % 60).toFixed(1);
		return `${m}:${sec.padStart(4, '0')}`;
	}
</script>

{#if s.renderMode === 'done' && s.renderResult?.output_path}
	<!-- svelte-ignore a11y_media_has_caption -- auto-generated
	     highlight reel, no caption source exists to attach -->
	<video
		class="player"
		controls
		bind:this={videoEl}
		ontimeupdate={() => (currentTime = videoEl?.currentTime ?? 0)}
		src={outputFileUrl(s.renderResult.output_path)}
	></video>

	{#if s.loadingManifest}
		<p class="dim">Loading timeline…</p>
	{:else if s.manifestError}
		<p class="error">{s.manifestError}</p>
	{:else if s.segmentLayout.length > 0}
		<div class="timeline-head">
			<span class="label">Timeline</span>
			<span class="dim mono">{s.segmentLayout.length} segments</span>
			<span class="dim approx-note">
				— seek is approximate when transitions/speed-ramps are on
			</span>
		</div>
		<div class="timeline" role="group" aria-label="Timeline segments">
			{#each s.segmentLayout as layout (layout.index)}
				<button
					type="button"
					class="seg"
					class:active={activeIndex === layout.index}
					class:selected={s.selectedSegmentIndex === layout.index}
					style="flex-basis: {((layout.endSecs - layout.startSecs) / totalSecs) * 100}%; background: {sourceColor(
						layout.segment.source_id
					)}"
					title="{layout.segment.source_id} · {fmtTime(layout.startSecs)}–{fmtTime(layout.endSecs)}"
					onclick={() => selectAndSeek(layout)}
				>
					<span class="seg-n">{layout.index + 1}</span>
				</button>
			{/each}
		</div>

		{#if selected}
			<div class="inspector">
				<div class="inspector-row">
					<span class="src-tag">
						<span class="swatch" style="background: {sourceColor(selected.segment.source_id)}"
						></span>
						<span class="mono">{selected.segment.source_id}</span>
					</span>
					<span class="dim mono">
						{fmtTime(selected.startSecs)}–{fmtTime(selected.endSecs)} ({selected.segment.duration_secs.toFixed(
							1
						)}s)
					</span>
					{#if selected.segment.score !== null}
						<span class="dim mono">score {selected.segment.score.toFixed(2)}</span>
					{/if}
				</div>
				<p class="reason">{selected.segment.reason}</p>
				<p class="dim mono chapter-path">{selected.segment.chapter_path}</p>
				<div class="inspector-actions">
					<button
						class="btn"
						disabled={s.feedbackBusy}
						onclick={() => s.moreOfSource(selected.segment.source_id)}
					>
						More of this source
					</button>
					<button
						class="btn"
						disabled={s.feedbackBusy}
						onclick={() => s.lessOfSource(selected.segment.source_id)}
					>
						Less of this source
					</button>
					<button class="btn danger" disabled={s.feedbackBusy} onclick={() => s.dropSegment(selected)}>
						Drop this segment
					</button>
				</div>
			</div>
		{/if}
	{/if}

	{#if s.loadingFrames}
		<p class="dim">Loading frames…</p>
	{:else if s.frameError}
		<p class="error">{s.frameError}</p>
	{:else if s.frames.length > 0}
		<div class="filmstrip">
			{#each s.frames as f, i (i)}
				<img src={f.data_uri} alt="frame at {f.t_secs.toFixed(1)}s" title="{f.t_secs.toFixed(1)}s" />
			{/each}
		</div>
	{/if}
{:else}
	<p class="dim">Render an output first.</p>
{/if}

<style>
	.player {
		width: 100%;
		max-height: 480px;
		border-radius: 8px;
		background: #000;
	}

	.dim {
		color: var(--text-faint);
		font-size: 13px;
	}

	.error {
		color: var(--bad);
		font-size: 13px;
		margin-top: 10px;
	}

	.mono {
		font-family: var(--mono);
	}

	.label {
		font-size: 11px;
		text-transform: uppercase;
		letter-spacing: 0.06em;
		color: var(--text-faint);
	}

	.timeline-head {
		display: flex;
		align-items: baseline;
		gap: 10px;
		margin-top: 16px;
		margin-bottom: 6px;
	}

	.approx-note {
		font-size: 11.5px;
	}

	.timeline {
		display: flex;
		height: 34px;
		border-radius: 6px;
		overflow: hidden;
		border: 1px solid var(--line);
	}

	.seg {
		position: relative;
		height: 100%;
		border: none;
		border-right: 1px solid rgba(0, 0, 0, 0.35);
		cursor: pointer;
		display: flex;
		align-items: center;
		justify-content: center;
		padding: 0;
		min-width: 3px;
	}

	.seg:last-child {
		border-right: none;
	}

	.seg:hover {
		filter: brightness(1.15);
	}

	.seg.active {
		box-shadow: inset 0 0 0 2px #fff;
	}

	.seg.selected {
		outline: 2px solid var(--accent);
		outline-offset: -2px;
		z-index: 1;
	}

	.seg-n {
		font-family: var(--mono);
		font-size: 9.5px;
		color: rgba(0, 0, 0, 0.55);
		font-weight: 600;
	}

	.inspector {
		margin-top: 12px;
		background: var(--panel-alt);
		border: 1px solid var(--line);
		border-radius: 8px;
		padding: 12px 14px;
	}

	.inspector-row {
		display: flex;
		align-items: center;
		gap: 14px;
		flex-wrap: wrap;
		font-size: 12.5px;
	}

	.src-tag {
		display: inline-flex;
		align-items: center;
		gap: 6px;
	}

	.swatch {
		width: 9px;
		height: 9px;
		border-radius: 2px;
		flex-shrink: 0;
	}

	.reason {
		margin: 8px 0 2px;
		font-size: 12.5px;
		color: var(--text-dim);
	}

	.chapter-path {
		margin: 0 0 10px;
		font-size: 11.5px;
		word-break: break-all;
	}

	.inspector-actions {
		display: flex;
		gap: 8px;
		flex-wrap: wrap;
	}

	.btn {
		border: 1px solid var(--line);
		background: var(--panel);
		color: var(--text);
		padding: 6px 12px;
		border-radius: 6px;
		font-size: 12.5px;
		font-weight: 600;
		cursor: pointer;
	}

	.btn:disabled {
		opacity: 0.45;
		cursor: not-allowed;
	}

	.btn:hover:not(:disabled) {
		border-color: var(--accent);
		color: var(--accent);
	}

	.btn.danger:hover:not(:disabled) {
		border-color: var(--bad);
		color: var(--bad);
	}

	.filmstrip {
		display: flex;
		gap: 6px;
		overflow-x: auto;
		margin-top: 14px;
		padding-bottom: 4px;
	}

	.filmstrip img {
		height: 64px;
		border-radius: 4px;
		border: 1px solid var(--line);
		flex-shrink: 0;
	}
</style>
