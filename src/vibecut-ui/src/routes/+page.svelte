<script lang="ts">
	import { onMount } from 'svelte';
	import { appState as s } from '$lib/state.svelte';
	import type { FeedbackAction } from '$lib/api';
	import PreviewTimeline from '$lib/components/PreviewTimeline.svelte';
	import BriefInput from '$lib/components/BriefInput.svelte';
	import CompareHistory from '$lib/components/CompareHistory.svelte';

	onMount(() => {
		void s.loadVibes();
		void s.loadSessions();
	});

	const quickActions: { label: string; action: FeedbackAction }[] = [
		{ label: 'Calmer', action: { kind: 'calmer' } },
		{ label: 'Punchier', action: { kind: 'punchier' } },
		{ label: 'Shorter', action: { kind: 'scale_duration', factor: 0.75 } },
		{ label: 'Longer', action: { kind: 'scale_duration', factor: 1.25 } },
		{ label: 'Reroll', action: { kind: 'reroll' } }
	];

	function renderStateLabel(mode: string): string {
		switch (mode) {
			case 'queued':
				return 'queued…';
			case 'running':
				return 'rendering…';
			case 'done':
				return 'done';
			case 'failed':
				return 'failed';
			default:
				return 'idle';
		}
	}
</script>

<svelte:head>
	<title>Vibecut</title>
</svelte:head>

<div class="app">
	<header class="topbar">
		<div class="brand"><span class="dot"></span> Vibecut</div>
		<span class="subtitle">Sources → Vibe → Config → Render → Preview → History</span>
	</header>

	<main class="steps">
		<!-- 1. Sources -->
		<section class="card">
			<h2><span class="step">1</span> Sources</h2>
			<div class="row">
				<input
					class="text-input"
					placeholder="footage folder (relative to /footage)"
					bind:value={s.folder}
				/>
				<button class="btn primary" disabled={s.discovering} onclick={() => s.discover()}>
					{s.discovering ? 'Scanning…' : 'Discover'}
				</button>
			</div>
			{#if s.discoverError}
				<p class="error">{s.discoverError}</p>
			{/if}
			{#if s.discoverResult}
				<p class="summary">{s.discoverResult.summary}</p>
				{#if s.discoverResult.sources.length > 0}
					<div class="source-list">
						{#each s.discoverResult.sources as src (src.source_id)}
							<div class="source-chip">
								<span class="mono">{src.source_id}</span>
								<span class="dim">{src.n_chapters} ch · {src.total_duration_secs.toFixed(1)}s</span>
								{#if src.has_audio}<span class="badge good">audio</span>{/if}
							</div>
						{/each}
					</div>
				{/if}
			{/if}
		</section>

		<!-- 2. Vibe picker -->
		<section class="card">
			<h2><span class="step">2</span> Vibe</h2>
			{#if s.vibesError}
				<p class="error">{s.vibesError}</p>
			{/if}
			<div class="vibe-grid">
				{#each s.vibes as vibe (vibe.key)}
					<button
						type="button"
						class="vibe-card"
						class:selected={s.selectedVibe === vibe.key}
						onclick={() => (s.selectedVibe = vibe.key)}
					>
						<strong>{vibe.name}</strong>
						<span class="dim">{vibe.default_target_duration}</span>
						<p>{vibe.rationale}</p>
					</button>
				{/each}
			</div>
			<div class="row wrap">
				<input class="text-input" placeholder="title (optional)" bind:value={s.title} />
				<input
					class="text-input narrow"
					placeholder="duration, e.g. 3m"
					bind:value={s.targetDuration}
				/>
				<input class="text-input narrow mono" type="number" bind:value={s.seed} />
				<button
					class="btn primary"
					disabled={!s.selectedVibe || s.proposing}
					onclick={() => s.propose()}
				>
					{s.proposing ? 'Proposing…' : 'Propose'}
				</button>
			</div>
			{#if s.proposeError}
				<p class="error">{s.proposeError}</p>
			{/if}
			{#if s.proposeResult}
				<p class="summary">
					<span class="mono">{s.proposeResult.config_path}</span> — {s.proposeResult.rationale}
				</p>
			{:else}
				<BriefInput />
			{/if}
		</section>

		<!-- 3. Config editor -->
		<section class="card" class:disabled={!s.proposeResult}>
			<h2><span class="step">3</span> Config</h2>
			{#if s.proposeResult}
				<textarea
					class="config-editor mono"
					value={s.configToml}
					oninput={(e) => s.setConfigToml((e.target as HTMLTextAreaElement).value)}
					spellcheck="false"
				></textarea>
				<div class="row wrap">
					<button
						class="btn primary"
						disabled={!s.configDirty || s.savingConfig}
						onclick={() => s.saveConfig()}
					>
						{s.savingConfig ? 'Saving…' : s.configDirty ? 'Save edits' : 'Saved'}
					</button>
					<span class="label">Quick feedback</span>
					{#each quickActions as qa (qa.label)}
						<button
							class="chip"
							disabled={s.feedbackBusy}
							onclick={() => s.quickFeedback(qa.action)}
						>
							{qa.label}
						</button>
					{/each}
				</div>
				{#if s.configError}
					<p class="error">{s.configError}</p>
				{/if}
				{#if s.lastChanges.length > 0}
					<ul class="changes">
						{#each s.lastChanges as c, i (i)}
							<li><span class="mono">{c.field}</span>: {c.before} → {c.after}</li>
						{/each}
					</ul>
				{/if}
				<BriefInput />
			{:else}
				<p class="dim">Propose a vibe first.</p>
			{/if}
		</section>

		<!-- 4. Render -->
		<section class="card" class:disabled={!s.proposeResult}>
			<h2><span class="step">4</span> Render</h2>
			<div class="row wrap">
				<button
					class="btn primary"
					disabled={!s.proposeResult || s.renderMode === 'queued' || s.renderMode === 'running'}
					onclick={() => s.startRenderAsync()}
				>
					Render
				</button>
				<span class="render-state">
					<span class="pulse" class:active={s.renderMode === 'running'}></span>
					{renderStateLabel(s.renderMode)}
					{#if s.jobId}<span class="mono dim">· {s.jobId}</span>{/if}
				</span>
				{#if s.configChangedSinceRender && s.renderResult}
					<span class="badge warn">config changed — re-render to update Preview</span>
				{/if}
			</div>
			{#if s.renderError}
				<p class="error">{s.renderError}</p>
			{/if}
			{#if s.renderResult?.manifest_summary}
				{@const m = s.renderResult.manifest_summary}
				<p class="summary">
					{m.n_segments} segments · {m.n_sources} sources · {m.actual_duration_secs?.toFixed(1)}s
					(target {m.target_duration_secs?.toFixed(1)}s)
				</p>
			{/if}
		</section>

		<!-- 5. Preview + Timeline -->
		<section class="card" class:disabled={s.renderMode !== 'done'}>
			<h2><span class="step">5</span> Preview</h2>
			<PreviewTimeline />
		</section>

		<!-- 6. History + Compare -->
		<section class="card">
			<h2><span class="step">6</span> History</h2>
			<CompareHistory />
		</section>
	</main>
</div>

<style>
	.app {
		max-width: 960px;
		margin: 0 auto;
		padding: 0 20px 60px;
	}

	.topbar {
		display: flex;
		align-items: baseline;
		gap: 14px;
		padding: 18px 0;
		border-bottom: 1px solid var(--line);
		margin-bottom: 24px;
	}

	.brand {
		display: flex;
		align-items: center;
		gap: 8px;
		font-weight: 800;
		font-size: 16px;
	}

	.brand .dot {
		width: 9px;
		height: 9px;
		border-radius: 50%;
		background: var(--accent);
		box-shadow: 0 0 0 3px var(--accent-dim);
	}

	.subtitle {
		color: var(--text-faint);
		font-size: 13px;
	}

	.steps {
		display: flex;
		flex-direction: column;
		gap: 18px;
	}

	.card {
		background: var(--panel);
		border: 1px solid var(--line);
		border-radius: 10px;
		padding: 18px 20px;
	}

	.card.disabled {
		opacity: 0.5;
	}

	.card h2 {
		display: flex;
		align-items: center;
		gap: 10px;
		font-size: 14px;
		margin: 0 0 14px;
	}

	.step {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		width: 20px;
		height: 20px;
		border-radius: 5px;
		background: var(--panel-alt);
		border: 1px solid var(--line);
		font-family: var(--mono);
		font-size: 11px;
		color: var(--text-dim);
	}

	.row {
		display: flex;
		align-items: center;
		gap: 10px;
	}

	.row.wrap {
		flex-wrap: wrap;
	}

	.text-input {
		flex: 1;
		min-width: 160px;
		background: var(--panel-alt);
		border: 1px solid var(--line);
		border-radius: 6px;
		padding: 8px 12px;
		font-size: 13px;
	}

	.text-input.narrow {
		flex: 0 0 140px;
		min-width: 0;
	}

	.mono {
		font-family: var(--mono);
	}

	.btn {
		border: 1px solid var(--line);
		background: var(--panel-alt);
		color: var(--text);
		padding: 8px 16px;
		border-radius: 6px;
		font-size: 13px;
		font-weight: 600;
		cursor: pointer;
		white-space: nowrap;
	}

	.btn:disabled {
		opacity: 0.45;
		cursor: not-allowed;
	}

	.btn.primary {
		background: var(--accent);
		border-color: var(--accent);
		color: #1a0e06;
	}

	.btn.primary:hover:not(:disabled) {
		filter: brightness(1.08);
	}

	.chip {
		border: 1px solid var(--line);
		background: var(--panel-alt);
		color: var(--text);
		padding: 6px 12px;
		border-radius: 999px;
		font-size: 12.5px;
		font-weight: 600;
		cursor: pointer;
	}

	.chip:hover:not(:disabled) {
		border-color: var(--accent);
		color: var(--accent);
	}

	.chip:disabled {
		opacity: 0.45;
		cursor: not-allowed;
	}

	.label {
		font-size: 11px;
		text-transform: uppercase;
		letter-spacing: 0.06em;
		color: var(--text-faint);
	}

	.error {
		color: var(--bad);
		font-size: 13px;
		margin-top: 10px;
	}

	.summary {
		color: var(--text-dim);
		font-size: 13px;
		margin-top: 10px;
	}

	.dim {
		color: var(--text-faint);
		font-size: 13px;
	}

	.source-list {
		display: flex;
		flex-wrap: wrap;
		gap: 8px;
		margin-top: 12px;
	}

	.source-chip {
		display: flex;
		align-items: center;
		gap: 8px;
		background: var(--panel-alt);
		border: 1px solid var(--line);
		border-radius: 6px;
		padding: 6px 10px;
		font-size: 12px;
	}

	.badge {
		font-size: 10.5px;
		font-weight: 700;
		text-transform: uppercase;
		letter-spacing: 0.04em;
		padding: 2px 6px;
		border-radius: 4px;
	}

	.badge.good {
		background: rgba(92, 201, 138, 0.16);
		color: var(--good);
	}

	.badge.warn {
		background: rgba(227, 183, 86, 0.16);
		color: var(--warn);
	}

	.vibe-grid {
		display: grid;
		grid-template-columns: repeat(auto-fill, minmax(200px, 1fr));
		gap: 10px;
		margin-bottom: 14px;
	}

	.vibe-card {
		text-align: left;
		background: var(--panel-alt);
		border: 1px solid var(--line);
		border-radius: 8px;
		padding: 12px 14px;
		cursor: pointer;
		display: flex;
		flex-direction: column;
		gap: 4px;
	}

	.vibe-card.selected {
		border-color: var(--accent);
		box-shadow: 0 0 0 1px var(--accent);
	}

	.vibe-card strong {
		font-size: 13px;
	}

	.vibe-card p {
		margin: 4px 0 0;
		font-size: 12px;
		color: var(--text-dim);
		line-height: 1.4;
	}

	.config-editor {
		width: 100%;
		min-height: 260px;
		background: var(--ink);
		border: 1px solid var(--line);
		border-radius: 8px;
		padding: 14px;
		font-size: 12.5px;
		line-height: 1.5;
		color: var(--text);
		resize: vertical;
	}

	.changes {
		margin: 12px 0 0;
		padding-left: 18px;
		font-size: 12.5px;
		color: var(--text-dim);
	}

	.render-state {
		display: flex;
		align-items: center;
		gap: 8px;
		font-size: 13px;
		color: var(--text-dim);
	}

	.pulse {
		width: 7px;
		height: 7px;
		border-radius: 50%;
		background: var(--text-faint);
	}

	.pulse.active {
		background: var(--good);
		box-shadow: 0 0 0 3px rgba(92, 201, 138, 0.18);
		animation: pulse 2s ease-in-out infinite;
	}

	@media (prefers-reduced-motion: reduce) {
		.pulse.active {
			animation: none;
		}
	}

	@keyframes pulse {
		0%,
		100% {
			opacity: 1;
		}
		50% {
			opacity: 0.4;
		}
	}

	/* .player / .filmstrip now live in PreviewTimeline.svelte */
</style>
