<script lang="ts">
	import { tick, untrack } from 'svelte';
	import * as api from '$lib/api';
	import type { BrowseListing, BrowseRootInfo } from '$lib/api';
	import { moveHighlight, videoCount } from '$lib/browse';

	// Pick a footage folder anywhere on the computer's drives, one level at
	// a time. Keyboard: arrows move, Enter (or →) opens a folder, Backspace
	// (or ←) goes up, Escape closes; the focus goes back to the opener.
	let {
		open = $bindable(false),
		onpick
	}: {
		open?: boolean;
		onpick: (choice: { container_path: string; path: string; name: string; sample_video: string | null }) => void;
	} = $props();

	let dialog: HTMLDialogElement | undefined = $state();
	let listEl: HTMLElement | undefined = $state();
	let opener: HTMLElement | null = null;

	let roots = $state<BrowseRootInfo[]>([]);
	/** The folder shown; null: the list of drives. */
	let listing = $state<BrowseListing | null>(null);
	let loading = $state(false);
	let error = $state<string | null>(null);
	let active = $state(0);
	/** sample video → poster frame. */
	let thumbs = $state<Record<string, string>>({});
	let seq = 0;
	let abort: AbortController | null = null;

	interface Item {
		key: string;
		name: string;
		path: string;
		detail: string;
		kind: 'drive' | 'footage' | 'folder';
		sample: string | null;
		videos: string;
	}

	const items = $derived<Item[]>(
		listing
			? listing.folders.map((f) => ({
					key: f.path,
					name: f.name,
					path: f.path,
					detail: f.has_subfolders ? 'Has folders' : '',
					kind: 'folder' as const,
					sample: f.sample_video,
					videos: videoCount(f.n_videos, f.complete)
				}))
			: roots
					.filter((r) => r.available)
					.map((r) => ({
						key: r.path,
						name: r.kind === 'footage' ? 'Footage' : r.label,
						path: r.path,
						detail: r.kind === 'drive' ? (r.path === '/' ? 'This computer' : 'Drive') : r.path,
						kind: r.kind === 'footage' ? ('footage' as const) : r.kind === 'drive' ? ('drive' as const) : ('folder' as const),
						sample: null,
						videos: ''
					}))
	);

	const optId = (i: number) => `browse-opt-${i}`;
	const here = $derived(listing ? listing.path : 'Drives');
	const status = $derived(
		loading
			? 'Loading…'
			: error
				? error
				: listing
					? `${listing.path}: ${items.length} folder${items.length === 1 ? '' : 's'}${listing.n_videos ? `, ${videoCount(listing.n_videos)} here` : ''}${listing.truncated ? ' (some left out)' : ''}`
					: `${items.length} place${items.length === 1 ? '' : 's'} to look`
	);
	const choice = $derived(
		listing && listing.can_use
			? {
					container_path: listing.container_path,
					path: listing.path,
					name: listing.name,
					sample_video: listing.sample_video ?? listing.folders.find((f) => f.sample_video)?.sample_video ?? null
				}
			: null
	);

	$effect(() => {
		if (!dialog) return;
		if (open && !dialog.open) {
			opener = document.activeElement as HTMLElement | null;
			dialog.showModal();
			untrack(() => void start());
		}
		if (!open && dialog.open) dialog.close();
	});

	async function start() {
		await tick();
		listEl?.focus();
		if (roots.length === 0 || !listing) await loadRoots();
		else await go(listing.path, items[active]?.key);
	}

	async function loadRoots(focusKey?: string) {
		const my = ++seq;
		abort?.abort();
		loading = true;
		error = null;
		try {
			const r = await api.browseRoots();
			if (my !== seq) return;
			roots = r.roots;
			listing = null;
			active = Math.max(0, items.findIndex((x) => x.key === focusKey));
		} catch (e) {
			if (my !== seq) return;
			error = e instanceof api.ApiError ? e.describe() : String(e);
		} finally {
			if (my === seq) loading = false;
		}
	}

	async function go(path: string, focusKey?: string) {
		const my = ++seq;
		abort?.abort();
		const ctrl = new AbortController();
		abort = ctrl;
		loading = true;
		error = null;
		try {
			const l = await api.browse(path, ctrl.signal);
			if (my !== seq) return;
			listing = l;
			active = Math.max(0, items.findIndex((x) => x.key === focusKey));
			void loadThumbs(l);
		} catch (e) {
			if (my !== seq) return;
			if (e instanceof DOMException && e.name === 'AbortError') return;
			error = e instanceof api.ApiError ? e.describe() : String(e);
		} finally {
			if (my === seq) {
				loading = false;
				abort = null;
			}
		}
		await tick();
		scrollActive();
	}

	async function loadThumbs(l: BrowseListing) {
		const wanted = l.folders
			.map((f) => f.sample_video)
			.filter((v): v is string => !!v && !thumbs[v])
			.slice(0, 24);
		const queue = [...wanted];
		const worker = async () => {
			for (let v = queue.shift(); v !== undefined; v = queue.shift()) {
				// Moved on (`listing` holds a proxy of `l`, so compare paths).
				if (listing?.path !== l.path) return;
				try {
					const r = await api.inspectAt(v, [2]);
					const uri = r.frames_inline[0]?.data_uri;
					if (uri) thumbs = { ...thumbs, [v]: uri };
				} catch {
					/* a missing thumbnail is cosmetic */
				}
			}
		};
		await Promise.all([worker(), worker(), worker()]);
	}

	function up() {
		if (loading) return;
		if (listing?.parent) void go(listing.parent, listing.path);
		else if (listing) void loadRoots(listing.crumbs[0]?.path);
	}

	function openItem(i: number) {
		const it = items[i];
		if (!it || loading) return;
		void go(it.path);
	}

	function scrollActive() {
		document.getElementById(optId(active))?.scrollIntoView({ block: 'nearest' });
	}

	function onKey(e: KeyboardEvent) {
		if (e.altKey || e.ctrlKey || e.metaKey) return;
		const next = moveHighlight(e.key, active, items.length);
		if (next !== null) {
			e.preventDefault();
			active = next;
			scrollActive();
			return;
		}
		if (e.key === 'Enter' || e.key === 'ArrowRight') {
			e.preventDefault();
			openItem(active);
		} else if (e.key === 'Backspace' || e.key === 'ArrowLeft') {
			e.preventDefault();
			up();
		}
	}

	function onListClick(e: MouseEvent) {
		const li = (e.target as HTMLElement).closest<HTMLElement>('[data-i]');
		if (!li) return;
		active = Number(li.dataset.i);
		openItem(active);
	}

	function crumb(path: string | null) {
		if (loading) return;
		if (path === null) void loadRoots(listing?.crumbs[0]?.path);
		else void go(path);
		listEl?.focus();
	}

	function use() {
		if (!choice) return;
		onpick(choice);
		open = false;
	}

	function onClose() {
		open = false;
		abort?.abort();
		// Back to whatever opened the dialog (the browser may not).
		if (opener?.isConnected) opener.focus();
		opener = null;
	}
</script>

<dialog bind:this={dialog} class="sheet" aria-labelledby="browse-h" onclose={onClose}>
	<header class="head">
		<div>
			<p class="eyebrow">Your footage, anywhere</p>
			<h2 id="browse-h">Browse your drives</h2>
		</div>
		<button class="close" type="button" aria-label="Close" onclick={() => (open = false)}>✕</button>
	</header>

	<nav class="crumbs" aria-label="Folder path">
		<ol>
			<li>
				{#if listing}
					<button type="button" class="crumb" onclick={() => crumb(null)}>Drives</button>
				{:else}
					<span class="crumb current" aria-current="location">Drives</span>
				{/if}
			</li>
			{#each listing?.crumbs ?? [] as c, i (c.path)}
				<li>
					<span class="sep" aria-hidden="true">›</span>
					{#if i === (listing?.crumbs.length ?? 0) - 1}
						<span class="crumb current" aria-current="location">{c.name}</span>
					{:else}
						<button type="button" class="crumb" onclick={() => crumb(c.path)}>{c.name}</button>
					{/if}
				</li>
			{/each}
		</ol>
	</nav>

	<p class="path mono" title={here}>{here}</p>

	<ul
		bind:this={listEl}
		class="list"
		role="listbox"
		tabindex="0"
		aria-label={listing ? `Folders in ${listing.path}` : 'Drives'}
		aria-describedby="browse-keys"
		aria-busy={loading}
		aria-activedescendant={items.length ? optId(active) : undefined}
		onkeydown={onKey}
		onclick={onListClick}
	>
		{#each items as it, i (it.key)}
			{@const thumb = it.sample ? thumbs[it.sample] : undefined}
			<li id={optId(i)} role="option" aria-selected={i === active} class="row" class:active={i === active} data-i={i}>
				<span class="icon" class:has-thumb={!!thumb} aria-hidden="true">
					{#if thumb}
						<img src={thumb} alt="" />
					{:else if it.kind === 'drive'}
						<svg viewBox="0 0 24 24" width="20" height="20"
							><rect x="3" y="7" width="18" height="10" rx="2.5" fill="none" stroke="currentColor" stroke-width="1.8" /><circle
								cx="17"
								cy="12"
								r="1.3"
								fill="currentColor"
							/></svg
						>
					{:else}
						<svg viewBox="0 0 24 24" width="20" height="20"
							><path
								d="M3.5 7.5a2 2 0 0 1 2-2h4l2 2h7a2 2 0 0 1 2 2v7.5a2 2 0 0 1-2 2h-13a2 2 0 0 1-2-2z"
								fill="none"
								stroke="currentColor"
								stroke-width="1.8"
								stroke-linejoin="round"
							/></svg
						>
					{/if}
				</span>
				<span class="name">{it.name}</span>
				<span class="meta">
					{#if it.videos}<span class="videos">{it.videos}</span>{/if}
					{#if it.detail}<span>{it.detail}</span>{/if}
				</span>
				<span class="chev" aria-hidden="true">›</span>
			</li>
		{:else}
			{#if !loading && !error}
				<li class="no-folders" role="presentation">
					{listing ? 'No folders in here.' : 'No drives to show. See DOCKER.md, “Browsing your drives”.'}
				</li>
			{/if}
		{/each}
	</ul>

	<p class="sr-only" role="status">{status}</p>
	{#if error}<p class="error">{error}</p>{/if}
	<p id="browse-keys" class="keys">
		<kbd>↑</kbd><kbd>↓</kbd> move · <kbd>Enter</kbd> open · <kbd>Backspace</kbd> up · <kbd>Esc</kbd> close
	</p>

	<footer class="foot">
		<span class="here">
			{#if listing}
				{#if !listing.can_use}
					Open a folder on this drive
				{:else if listing.n_videos}
					<strong>{videoCount(listing.n_videos)}</strong> in this folder
				{:else}
					Videos in its folders count too
				{/if}
			{:else}
				Pick a drive
			{/if}
		</span>
		<button class="btn ghost" type="button" disabled={!listing || loading} onclick={up}>Up</button>
		<button class="btn primary" type="button" disabled={!choice || loading} onclick={use}>Use this folder</button>
	</footer>
</dialog>

<style>
	.sheet {
		width: min(640px, calc(100vw - 24px));
		max-height: min(760px, calc(100vh - 32px));
		padding: 0;
		border-radius: var(--r-xl);
		border: 1px solid var(--line-strong);
		background: rgba(14, 16, 23, 0.97);
		color: var(--text);
		box-shadow: var(--shadow-2);
		overflow: hidden;
		flex-direction: column;
	}

	.sheet[open] {
		display: flex;
		animation: pop var(--dur-3) var(--ease-spring) both;
	}

	.sheet::backdrop {
		background: rgba(3, 4, 7, 0.65);
		backdrop-filter: blur(8px);
	}

	.head {
		display: flex;
		align-items: flex-start;
		justify-content: space-between;
		gap: 12px;
		padding: 20px 22px 8px;
	}

	.head .eyebrow {
		margin: 0;
	}

	h2 {
		margin: 2px 0 0;
		font-size: var(--fs-xl);
		letter-spacing: -0.02em;
	}

	.close {
		flex: none;
		width: 34px;
		height: 34px;
		border-radius: 50%;
		border: 1px solid var(--line-strong);
		background: rgba(7, 8, 12, 0.6);
		cursor: pointer;
	}

	.crumbs ol {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 2px;
		margin: 0;
		padding: 0 22px;
		list-style: none;
		font-size: var(--fs-sm);
	}

	.crumbs li {
		display: inline-flex;
		align-items: center;
		gap: 2px;
		min-width: 0;
	}

	.crumb {
		background: none;
		border: none;
		padding: 4px 6px;
		border-radius: var(--r-sm);
		color: var(--cyan);
		cursor: pointer;
		max-width: 22ch;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.crumb:hover {
		background: rgba(255, 255, 255, 0.06);
	}

	.crumb.current {
		color: var(--text);
		font-weight: 700;
		cursor: default;
	}

	.sep {
		color: var(--text-faint);
	}

	.path {
		margin: 2px 22px 10px;
		font-size: var(--fs-xs);
		color: var(--text-faint);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.list {
		flex: 1;
		min-height: 180px;
		margin: 0 14px;
		padding: 6px;
		list-style: none;
		overflow-y: auto;
		overscroll-behavior: contain;
		border: 1px solid var(--line);
		border-radius: var(--r-md);
		background: rgba(255, 255, 255, 0.02);
	}

	.list[aria-busy='true'] {
		opacity: 0.6;
	}

	.list:focus-visible {
		outline: 2px solid var(--accent);
		outline-offset: 2px;
	}

	.row {
		display: grid;
		grid-template-columns: 52px 1fr auto 14px;
		align-items: center;
		gap: 12px;
		padding: 7px 10px;
		border-radius: 10px;
		cursor: pointer;
		border: 1px solid transparent;
	}

	.row:hover {
		background: rgba(255, 255, 255, 0.04);
	}

	.row.active {
		background: var(--grad-brand-soft);
		border-color: var(--line-strong);
	}

	.icon {
		display: grid;
		place-items: center;
		width: 52px;
		height: 32px;
		border-radius: 7px;
		background: var(--panel-alt);
		color: var(--text-dim);
		overflow: hidden;
	}

	.icon img {
		width: 100%;
		height: 100%;
		object-fit: cover;
	}

	.name {
		font-weight: 600;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.meta {
		display: flex;
		gap: 10px;
		font-size: var(--fs-xs);
		color: var(--text-faint);
		white-space: nowrap;
	}

	.videos {
		color: var(--lime);
	}

	.chev {
		color: var(--text-faint);
	}

	.no-folders {
		padding: 18px 12px;
		color: var(--text-dim);
	}

	.error {
		margin: 8px 22px 0;
		color: var(--bad);
	}

	.keys {
		margin: 8px 22px 0;
		font-size: var(--fs-xs);
		color: var(--text-faint);
	}

	kbd {
		font-family: var(--mono);
		font-size: 11px;
		padding: 1px 5px;
		margin-right: 2px;
		border: 1px solid var(--line-strong);
		border-radius: 5px;
	}

	.foot {
		display: flex;
		align-items: center;
		gap: 10px;
		padding: 14px 22px 18px;
	}

	.here {
		flex: 1;
		color: var(--text-dim);
		font-size: var(--fs-sm);
	}

	.here strong {
		color: var(--text);
	}

	@keyframes pop {
		from {
			transform: scale(0.96);
			opacity: 0;
		}
	}

	@media (max-width: 560px) {
		.row {
			grid-template-columns: 44px 1fr 14px;
		}
		.meta {
			display: none;
		}
		.keys {
			display: none;
		}
	}
</style>
