<script lang="ts">
	import { appState as s } from '$lib/state.svelte';
	import { sourceColor } from '$lib/colors';
	import { clock } from '$lib/vibes';
	import { keepFocus } from '$lib/focus';

	const sel = $derived(
		s.selectedSegmentIndex !== null ? (s.segmentLayout[s.selectedSegmentIndex] ?? null) : null
	);
	const sourceIds = $derived(s.manifest?.sources.map((x) => x.id) ?? []);
	const busy = $derived(s.feedbackBusy || s.renderMode === 'queued' || s.renderMode === 'running');
	const burst = $derived(sel?.segment.source_id.startsWith('burst:') ?? false);
	const sourceId = $derived(sel?.segment.source_id.replace(/^burst:/, '') ?? '');
	const maxScore = $derived(Math.max(0.0001, ...(s.manifest?.segments.map((g) => g.score ?? 0) ?? [0])));

	/** The picker's reason in plain words. Its reason string names the
	 *  scoring *weights* in play ("hilight+accel"), not what actually
	 *  fired, so this stays honest: what kind of pick it was, and how it
	 *  ranks, without claiming a cause the manifest doesn't record. */
	function why(reason: string, score: number | null): string {
		const r = reason.toLowerCase();
		if (burst || r.includes('beat-burst')) return 'Part of the rapid-fire burst at the song’s energy peak.';
		if (r.includes('image')) return 'One of your photos, gently animated.';
		if (r.includes('range')) return 'A moment you asked to always include.';
		if (r.includes('even')) return 'Placed to cover the whole clip evenly.';
		const rel = score !== null ? score / maxScore : 0;
		if (rel >= 0.9) return 'One of the most eventful moments in all your footage.';
		if (rel >= 0.6) return 'A standout moment from this clip.';
		return 'Picked to keep the story moving between the big moments.';
	}

	/** Close, handing the keyboard focus back to the moment's block on
	 *  the timeline rather than dropping it on the page. */
	function close() {
		const i = s.selectedSegmentIndex;
		s.selectedSegmentIndex = null;
		if (i !== null) document.querySelectorAll<HTMLElement>('.timeline .seg')[i]?.focus();
	}

	// An action re-renders, which closes this panel and clears the
	// timeline: the focus waits at the top of the page and comes back to
	// the moment in the same place once the new timeline is drawn.
	let lastIndex = 0;
	$effect(() => {
		if (s.selectedSegmentIndex !== null) lastIndex = s.selectedSegmentIndex;
	});
	function sameMoment(): HTMLElement | null {
		const segs = document.querySelectorAll<HTMLElement>('.timeline .seg');
		return segs.length > 0 ? segs[Math.min(lastIndex, segs.length - 1)] : null;
	}

	async function act(kind: 'more' | 'less' | 'drop') {
		const g = sel;
		if (!g || busy) return;
		const label =
			kind === 'drop' ? `Dropped moment ${g.index + 1}` : kind === 'more' ? `More of clip ${sourceId}` : `Less of clip ${sourceId}`;
		await s.withUndo(label, async () => {
			if (kind === 'drop') await s.dropSegment(g);
			else if (kind === 'more') await s.moreOfSource(sourceId);
			else await s.lessOfSource(sourceId);
		});
	}
</script>

{#if sel}
	<aside class="inspector glass" aria-label="Selected moment" use:keepFocus={sameMoment}>
		<div class="top">
			<span class="swatch" style="--c: {sourceColor(sourceId, sourceIds)}" aria-hidden="true"></span>
			<div class="title">
				<strong>{burst ? 'Beat burst' : `Moment ${sel.index + 1}`}</strong>
				<span class="mono">
					{clock(sel.startSecs, true)}–{clock(sel.endSecs, true)} · clip {sourceId}
				</span>
			</div>
			<button class="btn ghost close" type="button" aria-label="Close moment details" onclick={close}>✕</button>
		</div>
		<p class="why serif">{why(sel.segment.reason, sel.segment.score)}</p>
		{#if sel.segment.score !== null && !burst}
			{@const strength = Math.round(Math.min(100, ((sel.segment.score ?? 0) / maxScore) * 100))}
			<div class="score" title="How strongly the picker rated this moment">
				<span class="eyebrow" id="strength-h">Moment strength</span>
				<span
					class="meter"
					role="meter"
					aria-labelledby="strength-h"
					aria-valuemin="0"
					aria-valuemax="100"
					aria-valuenow={strength}
					aria-valuetext="{strength}% of the strongest moment"><span style="width: {strength}%"></span></span
				>
			</div>
		{/if}
		<div class="actions">
			<!-- aria-disabled while busy: a disabled button drops the focus. -->
			<button class="btn" type="button" disabled={burst} aria-disabled={busy} onclick={() => act('more')}>More of this clip</button>
			<button class="btn" type="button" disabled={burst} aria-disabled={busy} onclick={() => act('less')}>Less of this clip</button>
			<button class="btn danger" type="button" aria-disabled={busy} onclick={() => act('drop')}>Drop this moment</button>
		</div>
		<details class="tech">
			<summary>Details</summary>
			<p class="mono">{sel.segment.reason}</p>
			<p class="mono">{sel.segment.chapter_path} @ {sel.segment.in_chapter_start_secs.toFixed(2)}s</p>
		</details>
	</aside>
{/if}

<style>
	.inspector {
		padding: 16px 18px;
		display: flex;
		flex-direction: column;
		gap: 10px;
		animation: rise var(--dur-2) var(--ease-out) both;
	}

	.top {
		display: flex;
		align-items: center;
		gap: 12px;
	}

	.swatch {
		width: 14px;
		height: 14px;
		border-radius: 4px;
		background: var(--c);
		box-shadow: 0 0 12px var(--c);
	}

	.title {
		display: flex;
		flex-direction: column;
		flex: 1;
	}

	.title .mono {
		font-size: var(--fs-xs);
		color: var(--text-faint);
	}

	.close {
		padding: 6px 10px;
	}

	.why {
		margin: 0;
		font-size: 21px;
		line-height: 1.3;
	}

	.score {
		display: flex;
		align-items: center;
		gap: 10px;
	}

	.meter {
		flex: 1;
		height: 6px;
		border-radius: 999px;
		background: rgba(255, 255, 255, 0.08);
		overflow: hidden;
	}

	.meter span {
		display: block;
		height: 100%;
		background: var(--grad-brand);
	}

	.actions {
		display: flex;
		flex-wrap: wrap;
		gap: 8px;
	}

	.tech summary {
		cursor: pointer;
		color: var(--text-faint);
		font-size: var(--fs-xs);
	}

	.tech p {
		margin: 6px 0 0;
		font-size: 11.5px;
		color: var(--text-faint);
		overflow-wrap: anywhere;
	}

	@keyframes rise {
		from {
			opacity: 0;
			transform: translateY(10px);
		}
	}
</style>
