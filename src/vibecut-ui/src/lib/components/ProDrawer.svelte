<script lang="ts">
	import { appState as s } from '$lib/state.svelte';
	import { outputFileUrl, type JobListEntry, type PlatformPreset } from '$lib/api';
	import ConfigWizard from './ConfigWizard.svelte';
	import { radiogroup } from '$lib/radiogroup';

	let { open = $bindable(false) }: { open?: boolean } = $props();
	let dialog: HTMLDialogElement | undefined = $state();

	$effect(() => {
		if (!dialog) return;
		if (open && !dialog.open) dialog.showModal();
		if (!open && dialog.open) dialog.close();
	});

	const dirty = $derived(s.structuredConfigDirty || s.configDirty);

	// Save whichever view holds the edits (not just the visible tab), then
	// render. Only this save's own errors count.
	const bothDirty = $derived(s.structuredConfigDirty && s.configDirty);

	// ---- Renders list: recent jobs (renders and platform versions),
	// refreshed whenever it's shown and whenever a render starts or ends
	// while it is.
	let rendersOpen = $state(false);
	$effect(() => {
		void s.renderMode;
		if (open && rendersOpen) void s.loadJobs();
	});

	const JOB_LABEL: Record<JobListEntry['state'], string> = {
		queued: 'Queued',
		running: 'Rendering',
		done: 'Done',
		failed: 'Failed',
		cancelled: 'Stopped'
	};

	const KIND_LABEL: Record<JobListEntry['kind'], string> = { render: 'Render', reformat: 'Reformat' };

	const PRESET_LABEL: Record<PlatformPreset, string> = {
		youtube: 'YouTube',
		reels: 'Reels',
		tiktok: 'TikTok',
		shorts: 'Shorts',
		square: 'Square',
		whatsapp: 'WhatsApp',
		master: 'Master'
	};

	function stateLabel(j: JobListEntry): string {
		if (j.state === 'running' && j.kind === 'reformat') return 'Making';
		return JOB_LABEL[j.state];
	}

	/** The finished file: a render's kept version, a reformat's version. */
	function jobFile(j: JobListEntry): string | undefined {
		if (j.state !== 'done') return undefined;
		return j.kind === 'render' ? (j.version_path ?? j.output_path) : j.output_path;
	}

	/** "proposal_coastline_…" for a render, "Reels version of coast.mp4" for a reformat. */
	function jobTitle(j: JobListEntry): string {
		if (j.kind === 'reformat') {
			const from = j.source_path.split('/').pop() ?? j.source_path;
			return `${PRESET_LABEL[j.preset] ?? j.preset}${j.fit ? ` (${j.fit})` : ''} version of ${from}`;
		}
		return projectName(j.config_path);
	}

	function isFinished(j: JobListEntry): boolean {
		return j.state === 'done' || j.state === 'failed' || j.state === 'cancelled';
	}

	function clockTime(iso: string): string {
		const d = new Date(iso);
		return Number.isNaN(d.getTime()) ? '' : d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
	}

	function took(j: JobListEntry): string {
		if (!('elapsed_ms' in j) || (j.state === 'cancelled' && !j.started_at)) return '';
		const secs = Math.round(j.elapsed_ms / 1000);
		return ` · ${secs < 60 ? `${secs} s` : `${Math.floor(secs / 60)} min ${secs % 60} s`}`;
	}

	/** `/projects/proposal_coastline_20261108T101500.toml` → `proposal_coastline_20261108T101500`. */
	function projectName(path: string): string {
		return (path.split('/').pop() ?? path).replace(/\.toml$/, '') || '—';
	}

	function belongsHere(j: JobListEntry): boolean {
		if (j.kind !== 'render') return false;
		const file = jobFile(j);
		return (
			!!file &&
			(j.config_path === s.proposeResult?.config_path || s.renderHistory.some((v) => v.output_path === file))
		);
	}

	async function openJob(j: JobListEntry) {
		if (await s.openJob(j)) open = false;
	}

	async function saveAndRender() {
		if (s.rendering) return;
		s.configError = null;
		s.configStructuredError = null;
		if (s.structuredConfigDirty) await s.saveStructuredConfig();
		else if (s.configDirty) await s.saveConfig();
		if (!s.configError && !s.configStructuredError) {
			open = false;
			void s.startRenderAsync();
		}
	}
</script>

<dialog bind:this={dialog} class="drawer" aria-labelledby="pro-h" onclose={() => (open = false)}>
	<header>
		<div>
			<p class="eyebrow">Advanced</p>
			<h2 id="pro-h">Pro settings</h2>
		</div>
		<button class="btn ghost" type="button" aria-label="Close Pro settings" onclick={() => (open = false)}>✕</button>
	</header>

	{#if s.proposeResult}
		<p class="path mono">{s.proposeResult.config_path}</p>
		<div class="tabs" role="tablist" aria-label="Settings view" use:radiogroup>
			<button
				type="button"
				role="tab"
				id="pro-tab-guided"
				aria-controls="pro-panel"
				aria-selected={s.configMode === 'guided'}
				class:active={s.configMode === 'guided'}
				onclick={() => (s.configMode = 'guided')}>Guided</button
			>
			<button
				type="button"
				role="tab"
				id="pro-tab-raw"
				aria-controls="pro-panel"
				aria-selected={s.configMode === 'raw'}
				class:active={s.configMode === 'raw'}
				onclick={() => (s.configMode = 'raw')}>Raw TOML</button
			>
		</div>

		<div class="body" role="tabpanel" id="pro-panel" aria-labelledby="pro-tab-{s.configMode}">
			{#if s.configMode === 'guided'}
				<ConfigWizard />
			{:else}
				<label class="sr-only" for="raw-toml">Project TOML</label>
				<textarea
					id="raw-toml"
					class="raw mono"
					value={s.configToml}
					oninput={(e) => s.setConfigToml((e.target as HTMLTextAreaElement).value)}
					spellcheck="false"
				></textarea>
				{#if s.configError}<p class="error" role="alert">{s.configError}</p>{/if}
			{/if}
		</div>

		<footer>
			<span class="state">{dirty ? 'Unsaved changes' : 'All changes saved'}</span>
			{#if bothDirty}<span class="warn">Both tabs have unsaved edits; the Guided ones will be saved.</span>{/if}
			<button
				class="btn primary"
				type="button"
				disabled={s.savingConfig || s.rendering}
				title={s.rendering ? 'Wait for the current render to finish' : undefined}
				onclick={saveAndRender}
			>
				{s.rendering ? 'Rendering…' : dirty ? 'Save & re-render' : 'Re-render'}
			</button>
		</footer>

		<details class="logs renders" bind:open={rendersOpen}>
			<summary>Renders</summary>
			<button class="btn ghost small" type="button" onclick={() => s.loadJobs()} disabled={s.loadingJobs}>
				{s.loadingJobs ? 'Loading…' : 'Refresh'}
			</button>
			{#if s.jobsError}<p class="error">{s.jobsError}</p>{/if}
			<ul>
				{#each s.jobs as j (j.job_id)}
					{@const file = jobFile(j)}
					<li>
						<span class="job-state {j.state}">{stateLabel(j)}{j.cancel_requested && !isFinished(j) ? ' · stopping' : ''}</span>
						<span class="kind {j.kind}">{KIND_LABEL[j.kind] ?? j.kind}</span>
						<span class="when mono">{clockTime(j.submitted_at)}{took(j)}</span>
						<span class="what" title={j.kind === 'reformat' ? j.output_path : j.config_path}>{jobTitle(j)}</span>
						{#if j.state === 'queued' || j.state === 'running'}
							<button class="btn ghost small" type="button" disabled={j.cancel_requested} onclick={() => s.stopJob(j.job_id)}
								>Stop</button
							>
						{:else if file}
							{#if belongsHere(j)}
								<button class="btn ghost small" type="button" disabled={s.rendering} onclick={() => openJob(j)}>Open</button>
							{:else}
								<a class="btn ghost small" href={outputFileUrl(file)} target="_blank" rel="noopener">Watch</a>
							{/if}
						{/if}
					</li>
				{:else}
					<li class="dim">{s.loadingJobs ? 'Loading…' : 'No renders yet.'}</li>
				{/each}
			</ul>
		</details>

		<details class="logs">
			<summary>Session logs</summary>
			<button class="btn ghost small" type="button" onclick={() => s.loadSessions()} disabled={s.loadingSessions}>
				{s.loadingSessions ? 'Loading…' : 'Refresh'}
			</button>
			<ul>
				{#each s.sessions as f (f.path)}
					<li class="mono">{f.filename} <span>{(f.size_bytes / 1024).toFixed(1)} KB</span></li>
				{:else}
					<li class="dim">No session logs yet.</li>
				{/each}
			</ul>
		</details>
	{:else}
		<p class="dim">Make a video first — its settings appear here.</p>
	{/if}
</dialog>

<style>
	.drawer {
		margin: 0 0 0 auto;
		height: 100dvh;
		max-height: 100dvh;
		width: min(760px, 100vw);
		padding: 22px 24px;
		border: none;
		border-left: 1px solid var(--line-strong);
		background: rgba(11, 13, 19, 0.96);
		color: var(--text);
		display: flex;
		flex-direction: column;
		gap: 12px;
		overflow-y: auto;
	}

	.drawer:not([open]) {
		display: none;
	}

	.drawer[open] {
		animation: slide var(--dur-3) var(--ease-out) both;
	}

	.drawer::backdrop {
		background: rgba(3, 4, 7, 0.6);
		backdrop-filter: blur(6px);
	}

	header {
		display: flex;
		align-items: flex-start;
		justify-content: space-between;
	}

	h2 {
		margin: 2px 0 0;
		font-size: var(--fs-2xl);
		letter-spacing: -0.03em;
	}

	.eyebrow {
		margin: 0;
	}

	.path {
		margin: 0;
		font-size: var(--fs-xs);
		color: var(--text-faint);
		overflow-wrap: anywhere;
	}

	.tabs {
		display: inline-flex;
		gap: 4px;
		padding: 4px;
		border-radius: 999px;
		border: 1px solid var(--line);
		width: fit-content;
	}

	.tabs button {
		border: none;
		background: transparent;
		color: var(--text-dim);
		padding: 6px 14px;
		border-radius: 999px;
		font-weight: 600;
		cursor: pointer;
	}

	.tabs button.active {
		background: var(--text);
		color: var(--ink);
	}

	.body {
		flex: 1;
	}

	.raw {
		width: 100%;
		min-height: 55vh;
		background: #05060a;
		border: 1px solid var(--line);
		border-radius: var(--r-md);
		padding: 14px;
		font-size: 12.5px;
		line-height: 1.55;
		resize: vertical;
	}

	footer {
		position: sticky;
		bottom: -22px;
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 12px;
		padding: 14px 0;
		background: linear-gradient(to top, rgba(11, 13, 19, 1) 70%, rgba(11, 13, 19, 0));
	}

	.warn {
		color: var(--warn);
		font-size: var(--fs-xs);
	}

	.state {
		color: var(--text-faint);
		font-size: var(--fs-sm);
	}

	.logs summary {
		cursor: pointer;
		color: var(--text-dim);
	}

	.logs ul {
		list-style: none;
		padding: 0;
		margin: 8px 0 0;
		font-size: var(--fs-xs);
		color: var(--text-dim);
	}

	.logs li span {
		color: var(--text-faint);
		margin-left: 8px;
	}

	.renders li {
		display: flex;
		align-items: center;
		gap: 4px;
		padding: 4px 0;
		border-bottom: 1px solid var(--line);
	}

	.renders li .job-state {
		margin-left: 0;
		min-width: 82px;
		font-weight: 700;
	}

	.renders .job-state.running,
	.renders .job-state.queued {
		color: var(--cyan);
	}

	.renders .job-state.done {
		color: var(--good);
	}

	.renders .job-state.failed {
		color: var(--bad);
	}

	.renders .kind {
		font-size: var(--fs-xs);
		padding: 1px 6px;
		border-radius: 999px;
		border: 1px solid var(--line-strong);
		color: var(--text-dim);
	}

	.renders .kind.reformat {
		color: var(--cyan);
	}

	.renders .what {
		flex: 1;
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.renders li .small {
		margin-top: 0;
	}

	.small {
		padding: 4px 10px;
		font-size: var(--fs-xs);
		margin-top: 8px;
	}

	.dim {
		color: var(--text-faint);
	}

	.error {
		color: var(--bad);
	}

	@keyframes slide {
		from {
			transform: translateX(40px);
			opacity: 0;
		}
	}
</style>
