<script lang="ts">
	import { appState as s } from '$lib/state.svelte';
	import type { FeedbackAction } from '$lib/api';

	// Six one-tap changes. Each rewrites the project deterministically and
	// re-renders straight away — no settings to understand.
	const TWEAKS: { label: string; icon: string; hint: string; action: FeedbackAction | 'song' }[] = [
		{ label: 'Shorter', icon: '⇤', hint: 'Cut it down by a quarter', action: { kind: 'scale_duration', factor: 0.75 } },
		{ label: 'Longer', icon: '⇥', hint: 'A quarter longer', action: { kind: 'scale_duration', factor: 1.25 } },
		{ label: 'Calmer', icon: '◡', hint: 'Softer transitions, fewer whip-ups', action: { kind: 'calmer' } },
		{ label: 'More energy', icon: '⚡', hint: 'Snappier cuts, more whip-ups', action: { kind: 'punchier' } },
		{ label: 'New song', icon: '♪', hint: 'Try the next song in your library', action: 'song' },
		{ label: 'Shuffle', icon: '↻', hint: 'Different moments, same style', action: { kind: 'reroll' } }
	];

	// Tweaks stay live during a render: picking one offers to stop it.
	// The offer is shared: keyboard shortcuts and song picks made
	// mid-render show up here too. Busy buttons (a change being written,
	// a stop in flight) are aria-disabled, not disabled: a disabled button
	// drops the keyboard focus onto the page the moment it's pressed.
	const busy = $derived(s.feedbackBusy || s.stopping);
	const offer = $derived(s.pendingChange?.from === 'tweaks' ? s.pendingChange : null);

	function tryRun(t: (typeof TWEAKS)[number] | 'mix') {
		if (busy) return;
		if (t === 'mix') s.requestChange('Another mix', () => s.anotherMix());
		else s.requestChange(t.label, () => run(t));
	}

	function nextSong(): string | null {
		if (s.tracks.length === 0) return null;
		const cur = s.effectiveTrack?.path;
		const i = s.tracks.findIndex((t) => t.path === cur);
		return s.tracks[(i + 1) % s.tracks.length].path;
	}

	function run(t: (typeof TWEAKS)[number]) {
		if (t.action === 'song') {
			const p = nextSong();
			if (p) void s.changeSong(p);
		} else {
			void s.tweakAndRender(t.action, t.label);
		}
	}

	/** "transition.duration: 400ms → 600ms" → friendlier text. */
	function friendly(field: string): string {
		const map: Record<string, string> = {
			'project.target_duration': 'Length',
			'transition.duration': 'Crossfade length',
			'transition.speed_ramp_probability': 'Whip-up frequency',
			'transition.speed_ramp_factor': 'Whip-up speed',
			'selection.variety': 'Variety',
			'soundtrack.beat_burst': 'Beat burst',
			'project.seed': 'Shuffle seed',
			'soundtrack.file': 'Song',
			'soundtrack.files': 'Song'
		};
		return map[field] ?? field;
	}

	/** "22500ms" → "22.5 s", "0.70" stays; long paths → file name. */
	function value(v: string): string {
		const ms = /^(\d+(?:\.\d+)?)ms$/.exec(v.trim());
		if (ms) return `${(Number(ms[1]) / 1000).toFixed(Number(ms[1]) % 1000 ? 1 : 0)} s`;
		const sec = /^(\d+(?:\.\d+)?)s$/.exec(v.trim());
		if (sec) return `${sec[1]} s`;
		if (v.includes('/')) return v.split('/').pop() ?? v;
		return v;
	}
</script>

<div class="tweaks" role="group" aria-labelledby="tweaks-h">
	<p class="eyebrow" id="tweaks-h">Make it…</p>
	<button class="mix" type="button" aria-disabled={busy} onclick={() => tryRun('mix')}>
		<span class="mix-icon" aria-hidden="true">⤨</span>
		<span><strong>Try another mix</strong><small>A different vibe with its own best song</small></span>
	</button>
	<div class="grid">
		{#each TWEAKS as t (t.label)}
			<button
				type="button"
				class="tweak"
				disabled={t.action === 'song' && s.tracks.length < 2}
				aria-disabled={busy}
				title={t.hint}
				onclick={() => tryRun(t)}
			>
				<span class="icon" aria-hidden="true">{t.icon}</span>
				<span>{t.label}</span>
			</button>
		{/each}
	</div>
	{#if offer}
		<div class="offer" role="status">
			<span>{s.rendering ? 'Still rendering the last change.' : 'The last change is ready.'}</span>
			<button class="btn primary small" type="button" disabled={s.stopping} onclick={() => s.applyPendingChange()}>
				{s.stopping
					? 'Stopping…'
					: `${s.rendering ? 'Stop and apply' : 'Apply'} “${offer.label}”${s.rendering ? ' instead' : ''}`}
			</button>
			<button class="btn ghost small" type="button" disabled={s.stopping} onclick={() => (s.pendingChange = null)}
				>{s.rendering ? 'Keep rendering' : 'Never mind'}</button
			>
		</div>
	{/if}
	{#if s.lastChanges.length > 0}
		<ul class="changes" aria-label="Last change">
			{#each s.lastChanges as c, i (i)}
				<li><span>{friendly(c.field)}</span> <span class="mono">{value(c.before)} → {value(c.after)}</span></li>
			{/each}
		</ul>
	{/if}
	{#if s.configError}<p class="error" role="alert">{s.configError}</p>{/if}
</div>

<style>
	.tweaks {
		display: flex;
		flex-direction: column;
		gap: 10px;
	}

	.offer {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 8px;
		padding: 10px 12px;
		border-radius: var(--r-md);
		border: 1px solid var(--line-strong);
		color: var(--text-dim);
		font-size: var(--fs-sm);
	}

	.offer .small {
		padding: 6px 12px;
		font-size: var(--fs-xs);
	}

	.eyebrow {
		margin: 0;
	}

	.mix {
		display: flex;
		align-items: center;
		gap: 12px;
		padding: 12px 14px;
		border-radius: var(--r-md);
		border: 1px solid rgba(255, 61, 129, 0.35);
		background: var(--grad-brand-soft);
		text-align: left;
		cursor: pointer;
		transition: transform var(--dur-1) var(--ease-out);
	}

	.mix:hover:not(:disabled):not([aria-disabled='true']) {
		transform: translateY(-2px);
	}

	.mix:disabled,
	.mix[aria-disabled='true'] {
		opacity: 0.45;
		cursor: not-allowed;
	}

	.mix span {
		display: flex;
		flex-direction: column;
	}

	.mix small {
		color: var(--text-dim);
		font-size: var(--fs-xs);
	}

	.mix-icon {
		font-size: 22px;
	}

	.grid {
		display: grid;
		grid-template-columns: repeat(3, 1fr);
		gap: 8px;
	}

	.tweak {
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: 6px;
		padding: 14px 8px;
		border-radius: var(--r-md);
		border: 1px solid var(--line);
		background: rgba(255, 255, 255, 0.035);
		font-weight: 600;
		font-size: var(--fs-sm);
		cursor: pointer;
		transition:
			transform var(--dur-1) var(--ease-out),
			background var(--dur-1),
			border-color var(--dur-1);
	}

	.tweak:hover:not(:disabled):not([aria-disabled='true']) {
		background: rgba(255, 255, 255, 0.08);
		border-color: rgba(255, 61, 129, 0.5);
		transform: translateY(-2px);
	}

	.tweak:active:not(:disabled):not([aria-disabled='true']) {
		transform: scale(0.96);
	}

	.tweak:disabled,
	.tweak[aria-disabled='true'] {
		opacity: 0.4;
		cursor: not-allowed;
	}

	.icon {
		font-size: 20px;
		line-height: 1;
		background: var(--grad-brand);
		-webkit-background-clip: text;
		background-clip: text;
		color: transparent;
	}

	.changes {
		list-style: none;
		margin: 0;
		padding: 10px 12px;
		border-radius: var(--r-sm);
		background: rgba(255, 255, 255, 0.03);
		border: 1px dashed var(--line-strong);
		display: flex;
		flex-direction: column;
		gap: 4px;
		font-size: var(--fs-xs);
		color: var(--text-dim);
	}

	.changes li {
		display: flex;
		justify-content: space-between;
		gap: 10px;
	}

	.changes .mono {
		color: var(--text-faint);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.error {
		color: var(--bad);
		margin: 0;
		font-size: var(--fs-sm);
	}
</style>
