<script lang="ts">
	import { appState as s } from '$lib/state.svelte';
	import ConfigWizard from './ConfigWizard.svelte';

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
		<button class="btn ghost" type="button" aria-label="Close" onclick={() => (open = false)}>✕</button>
	</header>

	{#if s.proposeResult}
		<p class="path mono">{s.proposeResult.config_path}</p>
		<div class="tabs" role="tablist">
			<button
				type="button"
				role="tab"
				aria-selected={s.configMode === 'guided'}
				class:active={s.configMode === 'guided'}
				onclick={() => (s.configMode = 'guided')}>Guided</button
			>
			<button
				type="button"
				role="tab"
				aria-selected={s.configMode === 'raw'}
				class:active={s.configMode === 'raw'}
				onclick={() => (s.configMode = 'raw')}>Raw TOML</button
			>
		</div>

		<div class="body">
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
				{#if s.configError}<p class="error">{s.configError}</p>{/if}
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
