<script lang="ts">
	import { appState as s } from '$lib/state.svelte';

	let { onOpenPro, onNew }: { onOpenPro: () => void; onNew: () => void } = $props();

	const hasProject = $derived(s.proposeResult !== null);
	const rendering = $derived(s.renderMode === 'queued' || s.renderMode === 'running');
</script>

<header class="topbar">
	<button class="brand" type="button" onclick={onNew} aria-label="Vibecut — start a new video">
		<span class="mark" aria-hidden="true">
			<svg viewBox="0 0 32 32" width="30" height="30">
				<defs>
					<linearGradient id="vb-g" x1="0" y1="0" x2="1" y2="1">
						<stop offset="0" stop-color="#ff7a45" />
						<stop offset=".55" stop-color="#ff3d81" />
						<stop offset="1" stop-color="#7b5cff" />
					</linearGradient>
				</defs>
				<rect width="32" height="32" rx="10" fill="rgba(255,255,255,0.05)" />
				<path
					d="M9 9l7 14 7-14"
					fill="none"
					stroke="url(#vb-g)"
					stroke-width="3.4"
					stroke-linecap="round"
					stroke-linejoin="round"
				/>
			</svg>
			<span class="beat-dot"></span>
		</span>
		<span class="word">Vibecut</span>
	</button>

	<nav class="right" aria-label="App">
		{#if s.aiAvailable}
			<span class="pill ai" title="Free-text requests are available">
				<span class="spark" aria-hidden="true"></span> AI on
			</span>
		{/if}
		{#if hasProject}
			<button class="btn ghost" type="button" onclick={onOpenPro}>
				<svg width="16" height="16" viewBox="0 0 24 24" aria-hidden="true"
					><path
						fill="currentColor"
						d="M4 7h10v2H4V7zm12 0h4v2h-4V7zM4 15h4v2H4v-2zm6 0h10v2H10v-2zM14 5h2v6h-2V5zM8 13h2v6H8v-6z"
					/></svg
				>
				Pro settings
			</button>
			{#if !rendering}<button class="btn" type="button" onclick={onNew}>New video</button>{/if}
		{/if}
	</nav>
</header>

<style>
	.topbar {
		position: sticky;
		top: 0;
		z-index: 30;
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 16px;
		padding: 14px var(--gutter);
		background: linear-gradient(to bottom, rgba(7, 8, 12, 0.85), rgba(7, 8, 12, 0));
	}

	.brand {
		display: inline-flex;
		align-items: center;
		gap: 10px;
		background: none;
		border: none;
		padding: 4px;
		border-radius: 12px;
		cursor: pointer;
	}

	.mark {
		position: relative;
		display: inline-flex;
	}

	.beat-dot {
		position: absolute;
		right: -2px;
		top: -2px;
		width: 8px;
		height: 8px;
		border-radius: 50%;
		background: var(--magenta);
		box-shadow: 0 0 12px var(--magenta);
		animation: beat var(--beat) var(--ease-out) infinite;
	}

	@keyframes beat {
		0% {
			transform: scale(1.35);
			opacity: 1;
		}
		70%,
		100% {
			transform: scale(0.85);
			opacity: 0.55;
		}
	}

	.word {
		font-weight: 800;
		font-size: 19px;
		letter-spacing: -0.03em;
	}

	.right {
		display: flex;
		align-items: center;
		gap: 8px;
	}

	.ai {
		color: var(--lime);
		border-color: rgba(184, 243, 90, 0.3);
	}

	.spark {
		width: 6px;
		height: 6px;
		border-radius: 50%;
		background: var(--lime);
		box-shadow: 0 0 10px var(--lime);
	}

	@media (max-width: 560px) {
		.word {
			display: none;
		}
	}
</style>
