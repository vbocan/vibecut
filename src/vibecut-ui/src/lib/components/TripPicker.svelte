<script lang="ts">
	import { appState as s } from '$lib/state.svelte';
	import { prettyBytes } from '$lib/vibes';
	import { sourceColor } from '$lib/colors';
	import { radiogroup } from '$lib/radiogroup';

	let manual = $state('');
	let showManual = $state(false);

	function pick(folder: string) {
		s.selectFolder(folder);
		s.persist();
	}

	function relTime(iso: string | null): string {
		if (!iso) return '';
		const d = new Date(iso);
		const days = Math.round((Date.now() - d.getTime()) / 86_400_000);
		if (days <= 0) return 'Today';
		if (days === 1) return 'Yesterday';
		if (days < 30) return `${days} days ago`;
		return d.toLocaleDateString(undefined, { month: 'short', day: 'numeric', year: 'numeric' });
	}

	const sourceIds = $derived(s.discoverResult?.sources.map((x) => x.source_id) ?? []);
	const totalMin = $derived(((s.discoverResult?.total_secs ?? 0) / 60).toFixed(1));
	// One short announcement per scan, not the whole clip strip.
	const scanStatus = $derived(
		!s.folder
			? ''
			: s.discovering
				? 'Looking through your clips…'
				: s.discoverError
					? s.discoverError
					: s.discoverResult
						? `${s.discoverResult.n_sources} clip${s.discoverResult.n_sources === 1 ? '' : 's'}, ${totalMin} min of footage.`
						: ''
	);
</script>

<section class="act" aria-labelledby="trip-h">
	<div class="act-head">
		<span class="num" aria-hidden="true">1</span>
		<h2 id="trip-h">Pick your footage</h2>
		<button class="btn ghost small" type="button" aria-expanded={showManual} onclick={() => (showManual = !showManual)}>
			{showManual ? 'Hide' : 'Type a folder'}
		</button>
	</div>

	{#if showManual}
		<form
			class="manual"
			onsubmit={(e) => {
				e.preventDefault();
				if (manual.trim()) pick(manual.trim());
			}}
		>
			<label class="sr-only" for="manual-folder">Folder under /footage</label>
			<input id="manual-folder" placeholder="Folder under /footage, e.g. trips/alps" bind:value={manual} />
			<button class="btn" type="submit" disabled={!manual.trim()}>Open</button>
		</form>
	{/if}

	{#if s.loadingFootage && s.footage.length === 0}
		<div class="trips" aria-busy="true">
			{#each [0, 1, 2] as i (i)}<div class="trip skeleton"></div>{/each}
		</div>
	{:else if s.footageError}
		<p class="error">Couldn't list your footage — {s.footageError}</p>
	{:else if s.footage.length === 0}
		<div class="empty glass">
			<strong>No footage yet.</strong>
			<span>Copy your GoPro or DJI clips into a folder inside the <code>footage</code> mount, then
				<button class="link" type="button" onclick={() => s.loadFootage()}>refresh</button>.</span
			>
		</div>
	{:else}
		<div class="trips" role="radiogroup" aria-labelledby="trip-h" use:radiogroup>
			{#each s.footage as f, i (f.folder)}
				{@const poster = s.folderPosters[f.folder]}
				<button
					type="button"
					role="radio"
					aria-checked={s.folder === f.folder}
					class="trip"
					class:selected={s.folder === f.folder}
					style="--i: {i}"
					onclick={() => pick(f.folder)}
				>
					<span class="poster" class:loaded={!!poster}>
						{#if poster}<img src={poster} alt="" />{/if}
					</span>
					<span class="shade"></span>
					<span class="meta">
						<span class="name">{f.name}</span>
						<span class="facts"
							>{f.n_videos} clip{f.n_videos === 1 ? '' : 's'} · {prettyBytes(f.total_bytes)}{f.newest
								? ` · ${relTime(f.newest)}`
								: ''}</span
						>
					</span>
					{#if s.folder === f.folder}<span class="check" aria-hidden="true">✓</span>{/if}
				</button>
			{/each}
		</div>
	{/if}

	<p class="sr-only" role="status">{scanStatus}</p>
	{#if s.folder && (s.discovering || s.discoverResult || s.discoverError)}
		<div class="sources">
			{#if s.discovering}
				<p class="hint"><span class="spinner" aria-hidden="true"></span> Looking through your clips…</p>
			{:else if s.discoverError}
				<p class="error">{s.discoverError}</p>
			{:else if s.discoverResult}
				<p class="hint">
					<span
						><strong>{s.discoverResult.n_sources} clip{s.discoverResult.n_sources === 1 ? '' : 's'}</strong
						>, {totalMin} min of footage — Vibecut will find the best moments.</span
					>
				</p>
				<div class="strip">
					{#each s.discoverResult.sources as src, i (src.source_id)}
						{@const thumbs = s.sourceThumbs[src.source_id]}
						<div class="clip" style="--i: {i}; --c: {sourceColor(src.source_id, sourceIds)}">
							<div class="frames">
								{#if thumbs}
									{#each thumbs as t, k (k)}<img src={t} alt="" />{/each}
								{:else}
									<span class="frames-skeleton"></span>
								{/if}
							</div>
							<span class="clip-meta">
								<span class="dot"></span>
								<span class="mono">{src.source_id}</span>
								<span class="dur">{Math.round(src.total_duration_secs)}s</span>
								{#if !src.has_audio}<span class="muted" title="No sound in this clip">muted</span>{/if}
							</span>
						</div>
					{/each}
				</div>
			{/if}
		</div>
	{/if}
</section>

<style>
	.act {
		display: flex;
		flex-direction: column;
		gap: 16px;
	}

	.act-head {
		display: flex;
		align-items: center;
		gap: 12px;
	}

	.num {
		display: inline-grid;
		place-items: center;
		width: 30px;
		height: 30px;
		border-radius: 50%;
		font-family: var(--serif);
		font-style: italic;
		font-size: 20px;
		background: var(--grad-brand-soft);
		border: 1px solid var(--line-strong);
	}

	h2 {
		margin: 0;
		font-size: var(--fs-xl);
		font-weight: 700;
		letter-spacing: -0.02em;
		flex: 1;
	}

	.small {
		padding: 6px 12px;
		font-size: var(--fs-xs);
	}

	.manual {
		display: flex;
		gap: 8px;
	}

	.manual input {
		flex: 1;
		background: rgba(255, 255, 255, 0.04);
		border: 1px solid var(--line-strong);
		border-radius: 999px;
		padding: 9px 16px;
	}

	.trips {
		display: grid;
		grid-auto-flow: column;
		grid-auto-columns: minmax(220px, 280px);
		gap: 14px;
		overflow-x: auto;
		overflow-y: hidden;
		/* Room for the hover lift and selection glow inside the scroller. */
		padding: 10px 6px 22px;
		margin: -10px -6px -14px;
		scroll-snap-type: x mandatory;
		scrollbar-width: thin;
	}

	.trip {
		position: relative;
		aspect-ratio: 16 / 10;
		border-radius: var(--r-lg);
		overflow: hidden;
		border: 1px solid var(--line);
		background: var(--panel-alt);
		padding: 0;
		cursor: pointer;
		text-align: left;
		scroll-snap-align: start;
		box-shadow: var(--shadow-1);
		animation: rise var(--dur-3) var(--ease-out) both;
		animation-delay: calc(var(--i, 0) * 70ms);
		transition:
			transform var(--dur-2) var(--ease-out),
			box-shadow var(--dur-2) var(--ease-out),
			border-color var(--dur-2) var(--ease-out);
	}

	.trip:hover {
		transform: translateY(-4px);
		border-color: var(--line-strong);
	}

	.trip.selected {
		border-color: transparent;
		box-shadow: var(--glow);
		transform: translateY(-4px);
	}

	.poster {
		position: absolute;
		inset: 0;
		background: linear-gradient(135deg, #1b2030, #10131b);
	}

	.poster img {
		width: 100%;
		height: 100%;
		object-fit: cover;
		transform: scale(1.04);
		animation: kenburns 14s var(--ease-in-out) infinite alternate;
	}

	.trip:hover .poster img,
	.trip.selected .poster img {
		animation-duration: 6s;
	}

	.shade {
		position: absolute;
		inset: 0;
		background: linear-gradient(to top, rgba(5, 6, 10, 0.92), rgba(5, 6, 10, 0.1) 60%);
	}

	.meta {
		position: absolute;
		left: 16px;
		right: 16px;
		bottom: 14px;
		display: flex;
		flex-direction: column;
		gap: 2px;
	}

	.name {
		font-size: 20px;
		font-weight: 700;
		letter-spacing: -0.02em;
		line-height: 1.15;
	}

	.facts {
		font-size: var(--fs-xs);
		color: var(--text-dim);
	}

	.check {
		position: absolute;
		top: 12px;
		right: 12px;
		width: 28px;
		height: 28px;
		display: grid;
		place-items: center;
		border-radius: 50%;
		background: var(--grad-brand);
		color: #160a12;
		font-weight: 800;
		animation: pop var(--dur-2) var(--ease-spring) both;
	}

	.skeleton,
	.frames-skeleton {
		background: linear-gradient(100deg, #151923 30%, #1f2533 50%, #151923 70%);
		background-size: 300% 100%;
		animation: shimmer 1.4s linear infinite;
	}

	.empty {
		display: flex;
		flex-direction: column;
		gap: 4px;
		padding: 18px 20px;
		color: var(--text-dim);
	}

	.empty strong {
		color: var(--text);
	}

	.link {
		background: none;
		border: none;
		padding: 0;
		color: var(--cyan);
		text-decoration: underline;
		cursor: pointer;
	}

	.sources {
		display: flex;
		flex-direction: column;
		gap: 10px;
	}

	.hint {
		margin: 0;
		color: var(--text-dim);
		display: flex;
		align-items: center;
		gap: 8px;
		flex-wrap: wrap;
	}

	.hint strong {
		color: var(--text);
	}

	.strip {
		display: grid;
		grid-auto-flow: column;
		grid-auto-columns: 168px;
		gap: 10px;
		overflow-x: auto;
		padding-bottom: 6px;
		scrollbar-width: thin;
	}

	.clip {
		display: flex;
		flex-direction: column;
		gap: 6px;
		animation: rise var(--dur-3) var(--ease-out) both;
		animation-delay: calc(var(--i, 0) * 45ms);
	}

	.frames {
		display: grid;
		grid-template-columns: repeat(3, 1fr);
		height: 52px;
		border-radius: 10px;
		overflow: hidden;
		border: 1px solid var(--line);
		box-shadow: inset 0 -3px 0 var(--c);
	}

	.frames img {
		width: 100%;
		height: 100%;
		object-fit: cover;
		animation: fade var(--dur-2) var(--ease-out) both;
	}

	.frames-skeleton {
		grid-column: 1 / -1;
	}

	.clip-meta {
		display: flex;
		align-items: center;
		gap: 6px;
		font-size: var(--fs-xs);
		color: var(--text-dim);
	}

	.dot {
		width: 8px;
		height: 8px;
		border-radius: 50%;
		background: var(--c);
	}

	.dur {
		margin-left: auto;
	}

	.muted {
		color: var(--warn);
	}

	.spinner {
		width: 14px;
		height: 14px;
		border-radius: 50%;
		border: 2px solid rgba(255, 255, 255, 0.2);
		border-top-color: var(--magenta);
		animation: spin 0.8s linear infinite;
	}

	.error {
		color: var(--bad);
		margin: 0;
	}

	@keyframes rise {
		from {
			opacity: 0;
			transform: translateY(14px) scale(0.98);
		}
		to {
			opacity: 1;
			transform: none;
		}
	}

	@keyframes pop {
		from {
			transform: scale(0.4);
			opacity: 0;
		}
	}

	@keyframes fade {
		from {
			opacity: 0;
		}
	}

	@keyframes kenburns {
		from {
			transform: scale(1.04) translate(0, 0);
		}
		to {
			transform: scale(1.14) translate(-3%, -2%);
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

	@keyframes spin {
		to {
			transform: rotate(360deg);
		}
	}
</style>
