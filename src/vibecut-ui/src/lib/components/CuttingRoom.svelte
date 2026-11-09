<script lang="ts">
	import { onDestroy } from 'svelte';
	import { appState as s } from '$lib/state.svelte';
	import type { RenderProgress } from '$lib/api';

	let {
		compact = false,
		onBack,
		backLabel = 'Back'
	}: { compact?: boolean; onBack?: () => void; backLabel?: string } = $props();

	const STAGES: { key: RenderProgress['stage']; label: string }[] = [
		{ key: 'probing', label: 'Find clips' },
		{ key: 'music', label: 'Hear the music' },
		{ key: 'analysing', label: 'Watch footage' },
		{ key: 'picking', label: 'Pick moments' },
		{ key: 'clips', label: 'Cut clips' },
		{ key: 'joining', label: 'Stitch' },
		{ key: 'mixing', label: 'Mix music' },
		{ key: 'checking', label: 'Check the beat' }
	];

	const PREP: Record<string, string> = {
		scanning: 'Reading your clips',
		planning: 'Planning the edit',
		scoring: 'Matching the music'
	};

	let now = $state(Date.now());
	const tick = setInterval(() => (now = Date.now()), 500);
	onDestroy(() => clearInterval(tick));

	const p = $derived(s.renderProgress);
	const failed = $derived(s.makeStep === 'failed' || s.renderMode === 'failed');
	const headline = $derived(
		failed
			? 'That didn’t work'
			: s.stopping
				? 'Stopping…'
				: p
				? p.label
				: (PREP[s.makeStep] ?? (s.renderMode === 'queued' ? 'Getting ready' : 'Warming up'))
	);
	const overall = $derived(failed ? 0 : (p?.overall ?? 0));
	const pct = $derived(Math.round(overall * 100));
	const elapsed = $derived(s.renderStartedAt ? Math.max(0, (now - s.renderStartedAt) / 1000) : 0);
	const eta = $derived.by(() => {
		if (!p || overall < 0.12 || elapsed < 4) return null;
		const left = (elapsed / overall) * (1 - overall);
		return left < 60 ? `about ${Math.max(5, Math.round(left / 5) * 5)} s left` : `about ${Math.round(left / 60)} min left`;
	});
	const stageIndex = $derived(p ? STAGES.findIndex((x) => x.key === p.stage) : -1);
	const hasMusic = $derived(!!s.effectiveTrack);
	const stages = $derived(STAGES.filter((x) => hasMusic || (x.key !== 'music' && x.key !== 'mixing')));

	// The wall: every source thumbnail, repeated to fill the plane.
	const wall = $derived.by(() => {
		const all = Object.values(s.sourceThumbs).flat();
		if (all.length === 0) return [];
		const out: string[] = [];
		while (out.length < 48) out.push(...all);
		return out.slice(0, 48);
	});

	const errorText = $derived(s.makeError ?? s.renderError ?? 'Something went wrong.');

	// One tap stops a wrong render. Before the render itself starts
	// (reading clips, planning) there is nothing on the server to stop yet.
	const canStop = $derived(s.rendering && !s.stopping);
</script>

<!-- Screen readers hear the stage changes and the outcome, not the
     ticking clock or every percent. -->
<p class="sr-only" role="status">{failed ? `Render stopped. ${errorText}` : headline}</p>
<section class="room" class:compact>
	{#if !compact}
		<div class="wall" aria-hidden="true">
			<div class="plane">
				{#each wall as src, i (i)}
					<img {src} alt="" style="--d: {(i % 7) * 0.4}s" />
				{/each}
			</div>
		</div>
		<div class="veil" aria-hidden="true"></div>
	{/if}

	{#if !compact && !failed && s.effectiveTrack}
		<button
			class="music-toggle"
			type="button"
			aria-pressed={s.musicOn}
			onclick={() => s.toggleMusic()}
			title={s.musicOn ? 'Mute the music while you wait' : 'Play the music while you wait'}
		>
			<span class="eq" class:on={s.musicOn} aria-hidden="true"><i></i><i></i><i></i></span>
			<span class="serif">♪ {s.effectiveTrack.title}</span>
			<span class="state">{s.musicOn ? 'Music on' : 'Music off'}</span>
		</button>
	{/if}

	<div class="stage">
		<p class="eyebrow">{failed ? 'Render stopped' : compact ? 'Re-cutting' : 'Now cutting your video'}</p>

		{#key headline}
			<h2 class="headline" class:bad={failed}>{headline}</h2>
		{/key}

		{#if failed}
			<p class="err">{errorText}</p>
			<div class="actions">
				<button class="btn primary" type="button" onclick={() => (s.proposeResult ? s.startRenderAsync() : s.makeVideo())}
					>Try again</button
				>
				{#if onBack}<button class="btn" type="button" onclick={onBack}>{backLabel}</button>{/if}
			</div>
		{:else}
			<p class="detail">{p?.detail ?? (s.jobId ? 'Starting the render…' : 'This takes a minute or two.')}</p>

			<div class="meter">
				<div class="rings" aria-hidden="true">
					<span></span><span></span><span></span>
				</div>
				<div class="pct mono" aria-label="{pct} percent"><span class="num">{pct}</span><small>%</small></div>
			</div>

			<div
				class="bar"
				role="progressbar"
				aria-valuemin="0"
				aria-valuemax="100"
				aria-valuenow={pct}
				aria-label="Render progress"
			>
				<div class="fill" style="width: {Math.max(2, pct)}%"></div>
			</div>

			<ol class="stages">
				{#each stages as st (st.key)}
					{@const idx = STAGES.findIndex((x) => x.key === st.key)}
					<li class:done={stageIndex > idx} class:active={stageIndex === idx}>
						<span class="tick" aria-hidden="true">{stageIndex > idx ? '✓' : ''}</span>
						{st.label}
					</li>
				{/each}
			</ol>

			<p class="time mono">
				{Math.floor(elapsed / 60)}:{Math.floor(elapsed % 60)
					.toString()
					.padStart(2, '0')}{#if eta}&nbsp;·&nbsp;{eta}{/if}
			</p>

			<button
				class="btn danger stop"
				type="button"
				disabled={!canStop}
				aria-busy={s.stopping}
				title={s.rendering
					? s.renderHistory.length > 0
						? 'Stop this render and go back to the last version'
						: 'Stop this render'
					: 'You can stop it once the render starts'}
				onclick={() => s.stopRender()}
			>
				<span class="stop-icon" aria-hidden="true"></span>
				{s.stopping ? 'Stopping…' : 'Stop'}
			</button>
		{/if}
	</div>
</section>

<style>
	.room {
		position: relative;
		min-height: calc(100dvh - 70px);
		display: grid;
		place-items: center;
		overflow: hidden;
		isolation: isolate;
	}

	.room.compact {
		min-height: 0;
		height: 100%;
		background: rgba(7, 8, 12, 0.72);
		backdrop-filter: blur(10px);
		-webkit-backdrop-filter: blur(10px);
		border-radius: inherit;
	}

	/* ---- the wall of footage ---- */
	.wall {
		position: absolute;
		inset: -20%;
		z-index: -2;
		perspective: 1200px;
	}

	.plane {
		display: grid;
		grid-template-columns: repeat(8, 1fr);
		gap: 14px;
		transform: rotateX(28deg) rotateZ(-8deg) translateY(-6%);
		animation: pan 40s linear infinite;
	}

	.plane img {
		width: 100%;
		aspect-ratio: 16 / 9;
		object-fit: cover;
		border-radius: 10px;
		opacity: 0.55;
		animation: flicker calc(var(--beat) * 8) var(--ease-in-out) infinite alternate;
		animation-delay: var(--d);
	}

	.veil {
		position: absolute;
		inset: 0;
		z-index: -1;
		background:
			radial-gradient(50% 45% at 50% 50%, rgba(7, 8, 12, 0.55), rgba(7, 8, 12, 0.92) 70%),
			linear-gradient(to bottom, rgba(7, 8, 12, 0.4), rgba(7, 8, 12, 0.85));
	}

	.music-toggle {
		position: absolute;
		top: 18px;
		right: var(--gutter);
		display: inline-flex;
		align-items: center;
		gap: 10px;
		padding: 8px 14px;
		border-radius: 999px;
		border: 1px solid var(--line-strong);
		background: rgba(7, 8, 12, 0.6);
		backdrop-filter: blur(10px);
		cursor: pointer;
		z-index: 2;
	}

	.music-toggle .serif {
		font-size: 16px;
	}

	.music-toggle .state {
		font-size: var(--fs-xs);
		color: var(--text-faint);
	}

	.eq {
		display: inline-flex;
		align-items: flex-end;
		gap: 2px;
		height: 14px;
	}

	.eq i {
		width: 3px;
		height: 4px;
		border-radius: 1px;
		background: var(--text-faint);
	}

	.eq.on i {
		background: var(--cyan);
		animation: eq var(--beat) var(--ease-in-out) infinite alternate;
	}

	.eq.on i:nth-child(2) {
		animation-delay: calc(var(--beat) * -0.33);
	}
	.eq.on i:nth-child(3) {
		animation-delay: calc(var(--beat) * -0.66);
	}

	@keyframes eq {
		to {
			height: 14px;
		}
	}

	/* ---- centre stage ---- */
	.stage {
		width: min(760px, 92vw);
		display: flex;
		flex-direction: column;
		align-items: center;
		text-align: center;
		gap: 14px;
		padding: 40px 0;
	}

	.compact .stage {
		gap: 10px;
		padding: 20px;
	}

	.headline {
		margin: 0;
		font-size: clamp(34px, 5.6vw, 72px);
		font-weight: 800;
		letter-spacing: -0.04em;
		line-height: 1;
		animation: swap 600ms var(--ease-out) both;
	}

	.compact .headline {
		font-size: clamp(24px, 3vw, 36px);
	}

	.headline.bad {
		color: var(--bad);
	}

	.detail {
		margin: 0;
		font-family: var(--serif);
		font-size: 22px;
		color: var(--text-dim);
		min-height: 1.4em;
	}

	.meter {
		position: relative;
		display: grid;
		place-items: center;
		width: 190px;
		height: 190px;
		margin: 6px 0;
	}

	.compact .meter {
		width: 120px;
		height: 120px;
	}

	.rings span {
		position: absolute;
		inset: 0;
		border-radius: 50%;
		border: 1.5px solid rgba(255, 61, 129, 0.55);
		animation: ring calc(var(--beat) * 4) linear infinite;
	}

	.rings span:nth-child(2) {
		animation-delay: var(--beat);
	}
	.rings span:nth-child(3) {
		animation-delay: calc(var(--beat) * 2);
	}

	.pct {
		position: relative;
		display: flex;
		align-items: baseline;
		font-size: 64px;
		font-weight: 600;
		font-variant-numeric: tabular-nums;
	}

	.pct .num {
		letter-spacing: -0.04em;
		padding-right: 0.04em;
		background: var(--grad-brand);
		-webkit-background-clip: text;
		background-clip: text;
		color: transparent;
	}

	.compact .pct {
		font-size: 40px;
	}

	.pct small {
		font-size: 0.36em;
		margin-left: 4px;
		color: var(--magenta);
	}

	.bar {
		width: 100%;
		height: 10px;
		border-radius: 999px;
		background: rgba(255, 255, 255, 0.07);
		overflow: hidden;
		box-shadow: inset 0 0 0 1px var(--line);
	}

	.fill {
		height: 100%;
		border-radius: inherit;
		background: var(--grad-brand);
		background-size: 200% 100%;
		box-shadow: 0 0 24px rgba(255, 61, 129, 0.7);
		transition: width 900ms var(--ease-out);
		animation: flow 2.4s linear infinite;
	}

	.stages {
		list-style: none;
		margin: 6px 0 0;
		padding: 0;
		display: flex;
		flex-wrap: wrap;
		justify-content: center;
		gap: 6px;
	}

	.compact .stages {
		display: none;
	}

	.stages li {
		display: inline-flex;
		align-items: center;
		gap: 6px;
		padding: 5px 12px;
		border-radius: 999px;
		font-size: var(--fs-xs);
		font-weight: 600;
		color: var(--text-faint);
		border: 1px solid var(--line);
		transition: all var(--dur-2) var(--ease-out);
	}

	.stages li.done {
		color: var(--good);
		border-color: rgba(74, 222, 128, 0.35);
	}

	.stages li.active {
		color: var(--text);
		border-color: rgba(255, 61, 129, 0.7);
		background: rgba(255, 61, 129, 0.12);
		box-shadow: 0 0 20px -4px rgba(255, 61, 129, 0.8);
	}

	.tick {
		width: 10px;
	}

	.time {
		margin: 4px 0 0;
		color: var(--text-faint);
		font-size: var(--fs-sm);
	}

	.err {
		margin: 0;
		color: var(--text-dim);
		max-width: 620px;
		overflow-wrap: anywhere;
	}

	.actions {
		display: flex;
		gap: 10px;
	}

	.stop {
		display: inline-flex;
		align-items: center;
		gap: 8px;
		margin-top: 4px;
	}

	.stop-icon {
		width: 10px;
		height: 10px;
		border-radius: 2px;
		background: currentColor;
	}

	@keyframes pan {
		from {
			transform: rotateX(28deg) rotateZ(-8deg) translateY(-6%);
		}
		to {
			transform: rotateX(28deg) rotateZ(-8deg) translateY(-30%);
		}
	}

	@keyframes flicker {
		from {
			opacity: 0.3;
		}
		to {
			opacity: 0.7;
		}
	}

	@keyframes ring {
		0% {
			transform: scale(0.55);
			opacity: 0.9;
		}
		100% {
			transform: scale(1.25);
			opacity: 0;
		}
	}

	@keyframes flow {
		from {
			background-position: 0% 0;
		}
		to {
			background-position: -200% 0;
		}
	}

	@keyframes swap {
		from {
			opacity: 0;
			transform: translateY(16px);
			filter: blur(6px);
		}
	}
</style>
