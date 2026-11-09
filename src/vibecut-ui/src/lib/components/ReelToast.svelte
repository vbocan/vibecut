<script lang="ts">
	import { appState as s } from '$lib/state.svelte';
	import { outputFileUrl } from '$lib/api';
	import { beatScore, reelSeconds, reelTitle } from '$lib/watch';

	// "Your reel is ready" for a reel the footage watcher made by itself:
	// its poster frame and beat score, and one tap to open it in the Studio.
	// It stays until opened or dismissed — it's the news, not a hint.

	const e = $derived(s.reelReady);
	const secs = $derived(e ? reelSeconds(e.render) : null);
	const beat = $derived(e ? beatScore(e.render) : null);

	// Opening or dismissing removes the toast under the keyboard focus:
	// hand it back to where it came from.
	let returnTo: HTMLElement | null = null;
	function onFocusIn(ev: FocusEvent & { currentTarget: HTMLElement }) {
		const from = ev.relatedTarget as HTMLElement | null;
		if (!ev.currentTarget.contains(from)) returnTo = from;
	}
	function leave(toast: HTMLElement | null) {
		if (toast?.contains(document.activeElement) && returnTo?.isConnected) returnTo.focus();
		returnTo = null;
	}

	async function open(toast: HTMLElement | null) {
		if (!e || s.rendering) return;
		leave(toast);
		await s.openWatched(e);
	}
</script>

<p class="sr-only" role="status">{e ? `New reel ready: ${reelTitle(e)}${beat ? `, ${beat}` : ''}.` : ''}</p>
{#if e}
	{#key e.id}
		<div class="reel-toast" role="group" aria-label="New reel ready" onfocusin={onFocusIn}>
			<span class="poster" aria-hidden="true">
				{#if e.poster_path}<img src={outputFileUrl(e.poster_path)} alt="" />{/if}
				<span class="play"><svg viewBox="0 0 24 24" width="18" height="18"><path fill="currentColor" d="M8 5v14l11-7z" /></svg></span>
			</span>
			<div class="body">
				<span class="eyebrow"><span class="dot" aria-hidden="true"></span> New footage · reel ready</span>
				<strong class="title">{reelTitle(e)}</strong>
				<span class="facts">
					{#if secs != null}<span>{secs} s</span>{/if}
					{#if secs != null && beat}<span class="sep" aria-hidden="true">·</span>{/if}
					{#if beat}<span class="beat">{beat}</span>{/if}
				</span>
				<div class="actions">
					<button
						class="btn primary small"
						type="button"
						aria-disabled={s.rendering}
						title={s.rendering ? 'A render is running — open it once that has finished' : undefined}
						onclick={(ev) => open(ev.currentTarget.closest('.reel-toast'))}>Open in Studio</button
					>
					<button
						class="btn ghost small"
						type="button"
						onclick={(ev) => {
							leave(ev.currentTarget.closest('.reel-toast'));
							s.reelReady = null;
						}}>Later</button
					>
				</div>
			</div>
		</div>
	{/key}
{/if}

<style>
	.reel-toast {
		position: fixed;
		top: 76px;
		right: var(--gutter);
		z-index: 61;
		width: min(420px, calc(100vw - 2 * var(--gutter)));
		display: flex;
		gap: 14px;
		padding: 12px;
		border-radius: var(--r-lg);
		background: rgba(20, 23, 32, 0.94);
		border: 1px solid var(--line-strong);
		box-shadow: var(--shadow-2), var(--glow);
		backdrop-filter: var(--blur);
		-webkit-backdrop-filter: var(--blur);
		animation: drop var(--dur-3) var(--ease-spring) both;
	}

	.poster {
		position: relative;
		flex: none;
		width: 132px;
		aspect-ratio: 16 / 9;
		align-self: center;
		border-radius: 12px;
		overflow: hidden;
		background: linear-gradient(135deg, #1b2030, #10131b);
		display: grid;
		place-items: center;
	}

	.poster img {
		position: absolute;
		inset: 0;
		width: 100%;
		height: 100%;
		object-fit: cover;
		transition: transform var(--dur-3) var(--ease-out);
	}

	.reel-toast:hover .poster img {
		transform: scale(1.06);
	}

	.play {
		position: relative;
		width: 34px;
		height: 34px;
		display: grid;
		place-items: center;
		border-radius: 50%;
		background: rgba(7, 8, 12, 0.55);
		color: var(--text);
		backdrop-filter: blur(4px);
	}

	.body {
		flex: 1;
		min-width: 0;
		display: flex;
		flex-direction: column;
		gap: 2px;
	}

	.eyebrow {
		display: inline-flex;
		align-items: center;
		gap: 6px;
		color: var(--good);
	}

	.dot {
		width: 7px;
		height: 7px;
		border-radius: 50%;
		background: var(--good);
		box-shadow: 0 0 10px var(--good);
	}

	.title {
		font-size: 17px;
		letter-spacing: -0.015em;
		line-height: 1.25;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.facts {
		font-size: var(--fs-sm);
		color: var(--text-dim);
	}

	.beat {
		color: var(--lime);
		font-weight: 600;
	}

	.sep {
		color: var(--text-faint);
		margin: 0 4px;
	}

	.actions {
		display: flex;
		gap: 6px;
		margin-top: 8px;
	}

	.small {
		padding: 6px 12px;
		font-size: var(--fs-xs);
	}

	@media (max-width: 520px) {
		.reel-toast {
			top: 68px;
		}
		.poster {
			width: 96px;
		}
	}

	@keyframes drop {
		from {
			opacity: 0;
			transform: translateY(-16px) scale(0.97);
		}
	}
</style>
