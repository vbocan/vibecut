<script lang="ts">
	import { appState as s } from '$lib/state.svelte';
	import { outputFileUrl, type WatchEntry } from '$lib/api';
	import { prettyBytes } from '$lib/vibes';
	import { reelTitle, watchStatusText, watchTone } from '$lib/watch';

	// The footage watcher's list: trips and cards it found since it was
	// switched on, each with where it is — waiting for the copy, up next,
	// being cut, ready to open — straight from the server's event stream.

	const w = $derived(s.watch);
	const entries = $derived(w?.entries ?? []);
	const watched = $derived(
		(w?.roots ?? [])
			.filter((r) => r.exists)
			.map((r) => r.path)
			.join(', ')
	);
	/** Where finished reels are also posted, without the scheme. */
	const hookHost = $derived(w?.webhook?.replace(/^https?:\/\//, '') ?? null);

	// Announced when a status changes, not on every progress tick.
	const summary = $derived.by(() => {
		if (!w?.enabled) return entries.length ? 'Auto-reel is off.' : '';
		const n = (st: string) => entries.filter((e) => e.status === st).length;
		const parts = [
			n('waiting') && `${n('waiting')} still copying`,
			n('queued') + n('rendering') && `${n('queued') + n('rendering')} being made`,
			n('done') && `${n('done')} ready`,
			n('failed') && `${n('failed')} failed`
		].filter(Boolean);
		return parts.length ? `New footage: ${parts.join(', ')}.` : '';
	});

	function pct(e: WatchEntry): number {
		return Math.max(2, Math.round((e.progress ?? 0) * 100));
	}

	function facts(e: WatchEntry): string {
		const what = e.kind === 'card' ? 'Camera card' : 'Folder';
		const clips = `${e.n_videos} clip${e.n_videos === 1 ? '' : 's'}`;
		return `${what} · ${clips} · ${prettyBytes(e.total_bytes)}`;
	}

	function canRetry(e: WatchEntry): boolean {
		return e.status === 'failed' || e.status === 'stopped' || e.status === 'skipped';
	}
</script>

<section class="watch" aria-labelledby="watch-h">
	<div class="head">
		<span class="radar" class:on={w?.enabled} aria-hidden="true"><span></span></span>
		<div class="titles">
			<p class="eyebrow">
				{#if w?.enabled}Watching <span class="mono">{watched || 'your footage folder'}</span>{:else}Auto-reel is off{/if}
			</p>
			<h2 id="watch-h">{entries.length ? 'New footage found' : 'Watching for new footage'}</h2>
		</div>
		<div class="head-actions">
			{#if w?.enabled && s.notifyPermission === 'default'}
				<button class="btn ghost small" type="button" onclick={() => s.askNotifications()}>
					<svg width="14" height="14" viewBox="0 0 24 24" aria-hidden="true"
						><path
							fill="currentColor"
							d="M12 22a2.5 2.5 0 0 0 2.45-2h-4.9A2.5 2.5 0 0 0 12 22zm7-6V11a7 7 0 0 0-5.5-6.84V3a1.5 1.5 0 0 0-3 0v1.16A7 7 0 0 0 5 11v5l-2 2v1h18v-1l-2-2z"
						/></svg
					>
					Notify me here
				</button>
			{/if}
			{#if w && !w.enabled}
				<button class="btn small" type="button" onclick={() => s.setWatchEnabled(true)}>Turn on</button>
			{/if}
		</div>
	</div>

	<p class="sr-only" role="status">{summary}</p>

	{#if entries.length === 0}
		<p class="lede">
			Copy a trip into your footage folder or plug in a camera card. Vibecut waits for the copy to finish, makes
			the reel with the vibe and song it would pick, and lets you know{hookHost ? ` — here and through ${hookHost}` : ''}.
		</p>
	{:else}
		<ul class="list" aria-label="Footage the watcher found">
			{#each entries as e, i (e.id)}
				{@const tone = watchTone(e)}
				{@const name = e.status === 'done' ? reelTitle(e) : e.name}
				<li class="row {tone}" style="--i: {i}">
					<span class="thumb">
						{#if e.status === 'done' && e.poster_path}
							<img src={outputFileUrl(e.poster_path)} alt="" loading="lazy" />
							<span class="play" aria-hidden="true">
								<svg viewBox="0 0 24 24" width="16" height="16"><path fill="currentColor" d="M8 5v14l11-7z" /></svg>
							</span>
						{:else if e.kind === 'card'}
							<svg class="icon" viewBox="0 0 24 24" width="26" height="26" aria-hidden="true"
								><path
									fill="currentColor"
									d="M8 2h8l4 4v14a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2V6l4-4zm1 2v4h2V4H9zm3 0v4h2V4h-2zm3 0v4h2V5.5L15.5 4H15z"
								/></svg
							>
						{:else}
							<svg class="icon" viewBox="0 0 24 24" width="26" height="26" aria-hidden="true"
								><path
									fill="currentColor"
									d="M3 6a2 2 0 0 1 2-2h4.6l2 2H19a2 2 0 0 1 2 2v10a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V6zm7 4v6l5-3-5-3z"
								/></svg
							>
						{/if}
						{#if tone === 'busy'}<span class="pulse" aria-hidden="true"></span>{/if}
					</span>
					<span class="info">
						<span class="name" title={e.path}>{name}</span>
						<span class="facts">{facts(e)}</span>
						<span class="status {tone}">{watchStatusText(e)}</span>
						{#if e.status === 'rendering'}
							<span class="bar" aria-hidden="true"><span style="width: {pct(e)}%"></span></span>
						{/if}
					</span>
					<span class="actions">
						{#if e.status === 'done'}
							<button
								class="btn primary small"
								type="button"
								aria-label="Open {name} in the Studio"
								aria-disabled={s.rendering}
								title={s.rendering ? 'A render is running — open it once that has finished' : undefined}
								onclick={() => {
									if (!s.rendering) void s.openWatched(e);
								}}>Open</button
							>
						{:else if canRetry(e)}
							<button
								class="btn small"
								type="button"
								aria-label="{e.status === 'skipped' ? 'Make a reel of' : 'Try again with'} {name}"
								onclick={() => s.retryWatched(e.id)}>{e.status === 'skipped' ? 'Make it anyway' : 'Try again'}</button
							>
						{/if}
						{#if e.status !== 'rendering'}
							<button
								class="x"
								type="button"
								aria-label={e.status === 'waiting' || e.status === 'queued' ? `Don't make a reel of ${name}` : `Dismiss ${name}`}
								title={e.status === 'waiting' || e.status === 'queued' ? "Don't make a reel of this" : 'Dismiss'}
								onclick={() => s.dismissWatched(e.id)}>✕</button
							>
						{/if}
					</span>
				</li>
			{/each}
		</ul>
	{/if}
</section>

<style>
	.watch {
		display: flex;
		flex-direction: column;
		gap: 14px;
	}

	.head {
		display: flex;
		align-items: center;
		gap: 14px;
		flex-wrap: wrap;
	}

	.titles {
		flex: 1;
		min-width: 0;
	}

	.eyebrow {
		margin: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.eyebrow .mono {
		letter-spacing: 0;
		text-transform: none;
	}

	h2 {
		margin: 2px 0 0;
		font-size: var(--fs-xl);
		font-weight: 700;
		letter-spacing: -0.02em;
	}

	.head-actions {
		display: flex;
		gap: 8px;
	}

	.small {
		padding: 6px 12px;
		font-size: var(--fs-xs);
	}

	/* A radar blip: steady when off, sweeping when watching. */
	.radar {
		position: relative;
		width: 30px;
		height: 30px;
		flex: none;
		border-radius: 50%;
		border: 1px solid var(--line-strong);
		background: rgba(255, 255, 255, 0.03);
	}

	.radar span {
		position: absolute;
		inset: 10px;
		border-radius: 50%;
		background: var(--text-faint);
	}

	.radar.on span {
		background: var(--lime);
		box-shadow: 0 0 12px var(--lime);
	}

	.radar.on::after {
		content: '';
		position: absolute;
		inset: -1px;
		border-radius: 50%;
		border: 1px solid var(--lime);
		animation: ping 2.4s var(--ease-out) infinite;
	}

	.lede {
		margin: 0;
		max-width: 680px;
		color: var(--text-dim);
	}

	.list {
		list-style: none;
		margin: 0;
		padding: 0;
		display: flex;
		flex-direction: column;
		gap: 8px;
	}

	.row {
		display: flex;
		align-items: center;
		gap: 14px;
		padding: 10px 12px 10px 10px;
		border-radius: var(--r-md);
		border: 1px solid var(--line);
		background: rgba(255, 255, 255, 0.025);
		animation: rise var(--dur-3) var(--ease-out) both;
		animation-delay: calc(var(--i, 0) * 50ms);
		transition: border-color var(--dur-2) var(--ease-out);
	}

	.row.ok {
		border-color: rgba(74, 222, 128, 0.28);
	}

	.thumb {
		position: relative;
		flex: none;
		width: 96px;
		aspect-ratio: 16 / 9;
		border-radius: 10px;
		overflow: hidden;
		display: grid;
		place-items: center;
		background: linear-gradient(135deg, #1b2030, #10131b);
		color: var(--text-faint);
	}

	.thumb img {
		width: 100%;
		height: 100%;
		object-fit: cover;
		animation: fade var(--dur-2) var(--ease-out) both;
	}

	.play {
		position: absolute;
		width: 28px;
		height: 28px;
		display: grid;
		place-items: center;
		border-radius: 50%;
		background: rgba(7, 8, 12, 0.6);
		color: var(--text);
		backdrop-filter: blur(4px);
	}

	.busy .icon {
		color: var(--cyan);
	}

	.pulse {
		position: absolute;
		inset: 0;
		background: linear-gradient(100deg, transparent 30%, rgba(56, 214, 255, 0.18) 50%, transparent 70%);
		background-size: 300% 100%;
		animation: shimmer 1.6s linear infinite;
	}

	.info {
		flex: 1;
		min-width: 0;
		display: flex;
		flex-direction: column;
		gap: 1px;
	}

	.name {
		font-weight: 700;
		letter-spacing: -0.01em;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.facts {
		font-size: var(--fs-xs);
		color: var(--text-faint);
	}

	.status {
		font-size: var(--fs-sm);
		color: var(--text-dim);
	}

	.status.busy {
		color: var(--cyan);
	}

	.status.ok {
		color: var(--good);
	}

	.status.bad {
		color: var(--bad);
	}

	.bar {
		margin-top: 5px;
		height: 4px;
		border-radius: 999px;
		background: rgba(255, 255, 255, 0.08);
		overflow: hidden;
	}

	.bar span {
		display: block;
		height: 100%;
		border-radius: inherit;
		background: var(--grad-brand);
		transition: width var(--dur-3) var(--ease-out);
	}

	.actions {
		display: flex;
		align-items: center;
		gap: 4px;
		flex: none;
	}

	.x {
		border: none;
		background: transparent;
		color: var(--text-faint);
		cursor: pointer;
		padding: 6px 10px;
		border-radius: 999px;
	}

	.x:hover {
		color: var(--text);
		background: rgba(255, 255, 255, 0.06);
	}

	@media (max-width: 560px) {
		.row {
			display: grid;
			grid-template-columns: 76px minmax(0, 1fr);
			gap: 10px 12px;
		}
		.thumb {
			width: 76px;
		}
		.actions {
			grid-column: 1 / -1;
			justify-content: flex-end;
		}
	}

	@keyframes ping {
		from {
			transform: scale(1);
			opacity: 0.8;
		}
		to {
			transform: scale(1.9);
			opacity: 0;
		}
	}

	@keyframes rise {
		from {
			opacity: 0;
			transform: translateY(10px);
		}
	}

	@keyframes fade {
		from {
			opacity: 0;
		}
	}

	@keyframes shimmer {
		from {
			background-position: 100% 0;
		}
		to {
			background-position: -100% 0;
		}
	}
</style>
