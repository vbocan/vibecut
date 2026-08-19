<script lang="ts">
	import { appState as s } from '$lib/state.svelte';
	import { outputFileUrl } from '$lib/api';

	let sliderPos = $state(50);

	const canCompare = $derived(s.renderHistory.length >= 2);
	const a = $derived(s.compareA !== null ? s.renderHistory[s.compareA] : null);
	const b = $derived(s.compareB !== null ? s.renderHistory[s.compareB] : null);

	function fmtBytes(n: number): string {
		if (n < 1024) return `${n} B`;
		return `${(n / 1024).toFixed(1)} KB`;
	}

	function fmtModified(iso: string | null): string {
		if (!iso) return '—';
		return new Date(iso).toLocaleString();
	}
</script>

<div class="subsection">
	<div class="subhead">
		<span class="label">Compare renders</span>
	</div>
	{#if s.renderHistory.length === 0}
		<p class="dim">No renders yet this session — finish step 4 to start building compare history.</p>
	{:else if !canCompare}
		<p class="dim">One render so far — render again after a tweak to compare it against this one.</p>
	{:else}
		<div class="row wrap">
			<label class="pick">
				A
				<select bind:value={s.compareA}>
					{#each s.renderHistory as r, i (i)}
						<option value={i}>{r.label}</option>
					{/each}
				</select>
			</label>
			<label class="pick">
				B
				<select bind:value={s.compareB}>
					{#each s.renderHistory as r, i (i)}
						<option value={i}>{r.label}</option>
					{/each}
				</select>
			</label>
		</div>
		{#if a && b}
			<div class="ab-wrap">
				<!-- svelte-ignore a11y_media_has_caption -- comparison scratch videos, no caption source -->
				<video class="ab-video" src={outputFileUrl(b.output_path)} controls muted></video>
				<!-- svelte-ignore a11y_media_has_caption -->
				<video
					class="ab-video ab-top"
					src={outputFileUrl(a.output_path)}
					controls
					muted
					style="clip-path: inset(0 {100 - sliderPos}% 0 0)"
				></video>
				<span class="ab-badge ab-badge-a">A · {a.label}</span>
				<span class="ab-badge ab-badge-b">B · {b.label}</span>
			</div>
			<input type="range" class="ab-slider" min="0" max="100" bind:value={sliderPos} />
			<p class="dim approx-note">Drag to compare a frame — playback isn't synced between A and B.</p>
		{/if}
	{/if}
</div>

<div class="subsection">
	<div class="subhead">
		<span class="label">Session log</span>
		<button class="btn" onclick={() => s.loadSessions()} disabled={s.loadingSessions}>
			{s.loadingSessions ? 'Loading…' : 'Refresh'}
		</button>
	</div>
	{#if s.sessionsError}
		<p class="error">{s.sessionsError}</p>
	{:else if s.sessions.length === 0}
		<p class="dim">No session logs under /projects/sessions yet.</p>
	{:else}
		<ul class="session-list">
			{#each s.sessions as file (file.path)}
				<li>
					<span class="mono">{file.filename}</span>
					<span class="dim">{fmtBytes(file.size_bytes)} · {fmtModified(file.modified)}</span>
				</li>
			{/each}
		</ul>
	{/if}
</div>

<style>
	.subsection + .subsection {
		margin-top: 20px;
		padding-top: 16px;
		border-top: 1px solid var(--line);
	}

	.subhead {
		display: flex;
		align-items: center;
		gap: 10px;
		margin-bottom: 10px;
	}

	.label {
		font-size: 11px;
		text-transform: uppercase;
		letter-spacing: 0.06em;
		color: var(--text-faint);
	}

	.dim {
		color: var(--text-faint);
		font-size: 13px;
	}

	.approx-note {
		font-size: 11.5px;
		margin-top: 6px;
	}

	.error {
		color: var(--bad);
		font-size: 13px;
	}

	.mono {
		font-family: var(--mono);
		font-size: 12.5px;
	}

	.row {
		display: flex;
		align-items: center;
		gap: 10px;
	}

	.row.wrap {
		flex-wrap: wrap;
	}

	.pick {
		display: flex;
		align-items: center;
		gap: 6px;
		font-size: 12px;
		color: var(--text-dim);
		font-weight: 700;
	}

	.pick select {
		background: var(--panel-alt);
		border: 1px solid var(--line);
		color: var(--text);
		border-radius: 6px;
		padding: 6px 10px;
		font-size: 12.5px;
	}

	.btn {
		border: 1px solid var(--line);
		background: var(--panel-alt);
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

	.ab-wrap {
		position: relative;
		width: 100%;
		aspect-ratio: 16 / 9;
		border-radius: 8px;
		overflow: hidden;
		background: #000;
		margin-top: 12px;
	}

	.ab-video {
		position: absolute;
		inset: 0;
		width: 100%;
		height: 100%;
		object-fit: contain;
		background: #000;
	}

	.ab-top {
		pointer-events: none;
	}

	.ab-badge {
		position: absolute;
		top: 8px;
		z-index: 2;
		font-family: var(--mono);
		font-size: 10.5px;
		font-weight: 600;
		padding: 3px 7px;
		border-radius: 4px;
		background: rgba(20, 23, 28, 0.7);
		color: var(--text);
	}

	.ab-badge-a {
		left: 8px;
	}

	.ab-badge-b {
		right: 8px;
	}

	.ab-slider {
		width: 100%;
		margin-top: 8px;
		accent-color: var(--accent);
	}

	.session-list {
		list-style: none;
		margin: 0;
		padding: 0;
		display: flex;
		flex-direction: column;
		gap: 6px;
	}

	.session-list li {
		display: flex;
		align-items: center;
		gap: 10px;
		background: var(--panel-alt);
		border: 1px solid var(--line);
		border-radius: 6px;
		padding: 7px 11px;
		font-size: 12.5px;
	}
</style>
