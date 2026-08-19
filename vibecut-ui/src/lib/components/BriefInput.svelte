<script lang="ts">
	import { appState as s } from '$lib/state.svelte';

	const isFeedbackMode = $derived(s.proposeResult !== null);

	function onKeydown(e: KeyboardEvent) {
		if (e.key === 'Enter' && !e.shiftKey) {
			e.preventDefault();
			void s.submitBrief();
		}
	}
</script>

<div class="brief">
	<div class="brief-head">
		<span class="label">{isFeedbackMode ? 'Or describe the change' : 'Or describe what you want'}</span>
		<span class="dim">— requires ANTHROPIC_API_KEY on the server</span>
	</div>
	<div class="row">
		<input
			class="text-input"
			placeholder={isFeedbackMode
				? 'e.g. "calmer, and lean more on the drone shots"'
				: 'e.g. "kinetic kid reel, upbeat music, under 2 minutes"'}
			bind:value={s.briefText}
			disabled={s.briefBusy}
			onkeydown={onKeydown}
		/>
		<button
			class="btn primary"
			disabled={s.briefBusy || !s.briefText.trim()}
			onclick={() => s.submitBrief()}
		>
			{s.briefBusy ? 'Asking…' : 'Ask AI'}
		</button>
	</div>
	{#if s.briefError}
		<p class="error">{s.briefError}</p>
	{/if}
	{#if s.briefRationale}
		<p class="rationale">"{s.briefRationale}"</p>
	{/if}
</div>

<style>
	.brief {
		margin-top: 14px;
		padding-top: 14px;
		border-top: 1px dashed var(--line);
	}

	.brief-head {
		display: flex;
		align-items: baseline;
		gap: 8px;
		margin-bottom: 8px;
		flex-wrap: wrap;
	}

	.label {
		font-size: 11px;
		text-transform: uppercase;
		letter-spacing: 0.06em;
		color: var(--text-faint);
	}

	.dim {
		color: var(--text-faint);
		font-size: 11.5px;
	}

	.row {
		display: flex;
		align-items: center;
		gap: 10px;
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

	.error {
		color: var(--bad);
		font-size: 13px;
		margin: 8px 0 0;
	}

	.rationale {
		color: var(--text-dim);
		font-size: 12.5px;
		font-style: italic;
		margin: 8px 0 0;
	}
</style>
