<script lang="ts">
	import { onDestroy } from 'svelte';
	import { appState as s } from '$lib/state.svelte';

	// Shown once the re-render for the last change has finished; one Undo
	// restores the settings and the previous version instantly.
	let shownFor = $state<object | null>(null);
	let timer: ReturnType<typeof setTimeout> | null = null;

	const visible = $derived(
		!!s.undo && s.renderMode === 'done' && shownFor === s.undo
	);

	$effect(() => {
		const u = s.undo;
		if (u && s.renderMode === 'done' && shownFor !== u) {
			shownFor = u;
			if (timer) clearTimeout(timer);
			timer = setTimeout(() => {
				if (s.undo === u) s.undo = null;
			}, 12000);
		}
	});

	onDestroy(() => {
		if (timer) clearTimeout(timer);
	});
</script>

<div class="toasts" aria-live="polite">
	{#if visible && s.undo}
		<div class="toast" role="status">
			<span class="dot" aria-hidden="true"></span>
			<span class="msg">{s.undo.label}</span>
			<button class="undo" type="button" onclick={() => s.undoLast()}>Undo</button>
			<button class="x" type="button" aria-label="Dismiss" onclick={() => (s.undo = null)}>✕</button>
		</div>
	{/if}
</div>

<style>
	.toasts {
		position: fixed;
		left: 50%;
		bottom: max(20px, env(safe-area-inset-bottom));
		transform: translateX(-50%);
		z-index: 60;
		pointer-events: none;
	}

	.toast {
		pointer-events: auto;
		display: flex;
		align-items: center;
		gap: 12px;
		padding: 8px 8px 8px 16px;
		border-radius: 999px;
		background: rgba(20, 23, 32, 0.92);
		border: 1px solid var(--line-strong);
		box-shadow: var(--shadow-2);
		backdrop-filter: var(--blur);
		animation: up var(--dur-3) var(--ease-spring) both;
	}

	.dot {
		width: 8px;
		height: 8px;
		border-radius: 50%;
		background: var(--good);
		box-shadow: 0 0 10px var(--good);
	}

	.msg {
		font-weight: 600;
		white-space: nowrap;
	}

	.undo {
		border: none;
		border-radius: 999px;
		padding: 7px 16px;
		font-weight: 800;
		background: var(--text);
		color: var(--ink);
		cursor: pointer;
	}

	.x {
		border: none;
		background: transparent;
		color: var(--text-faint);
		cursor: pointer;
		padding: 6px 10px;
	}

	@keyframes up {
		from {
			opacity: 0;
			transform: translateY(20px) scale(0.96);
		}
	}
</style>
