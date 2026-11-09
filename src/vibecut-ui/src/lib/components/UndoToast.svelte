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
			held.focus = held.pointer = false;
			arm();
		}
	});

	// Short notices (a stopped render) fade on their own after a few seconds.
	let noticeTimer: ReturnType<typeof setTimeout> | null = null;
	$effect(() => {
		const n = s.notice;
		if (noticeTimer) clearTimeout(noticeTimer);
		if (n) {
			noticeTimer = setTimeout(() => {
				if (s.notice === n) s.notice = null;
			}, 5000);
		}
	});

	// The countdown waits while the toast has the pointer or keyboard
	// focus (WCAG 2.2.1), and starts over when both leave.
	const held = { focus: false, pointer: false };
	function arm() {
		const u = s.undo;
		if (timer) clearTimeout(timer);
		timer = null;
		if (!u || held.focus || held.pointer) return;
		timer = setTimeout(() => {
			if (s.undo === u) s.undo = null;
		}, 12000);
	}
	function hold(kind: keyof typeof held, on: boolean) {
		held[kind] = on;
		arm();
	}

	// Undo / Dismiss remove the toast under the keyboard focus: hand it
	// back to where it came from instead of dropping it on the page.
	let returnTo: HTMLElement | null = null;
	function onFocusIn(e: FocusEvent & { currentTarget: HTMLElement }) {
		const from = e.relatedTarget as HTMLElement | null;
		if (!e.currentTarget.contains(from)) returnTo = from;
		hold('focus', true);
	}
	function onFocusOut(e: FocusEvent & { currentTarget: HTMLElement }) {
		if (!e.currentTarget.contains(e.relatedTarget as Node | null)) hold('focus', false);
	}
	function leave(toast: HTMLElement | null) {
		if (toast?.contains(document.activeElement) && returnTo?.isConnected) returnTo.focus();
		held.focus = held.pointer = false;
		returnTo = null;
	}

	onDestroy(() => {
		if (timer) clearTimeout(timer);
		if (noticeTimer) clearTimeout(noticeTimer);
	});
</script>

<!-- One status line each for screen readers (the text once, not the
     buttons too); the toasts themselves aren't second live regions. -->
<p class="sr-only" role="status">{s.notice ? s.notice.text : ''}</p>
<p class="sr-only" role="status">{visible && s.undo ? `${s.undo.label}. You can undo this.` : ''}</p>
<div class="toasts">
	{#if s.notice}
		<div class="toast notice" role="group" aria-label="Notice">
			<span class="dot info" aria-hidden="true"></span>
			<span class="msg">{s.notice.text}</span>
			<button class="x" type="button" aria-label="Dismiss notice" onclick={() => (s.notice = null)}>✕</button>
		</div>
	{/if}
	{#if visible && s.undo}
		<div
			class="toast"
			role="group"
			aria-label="Last change"
			onfocusin={onFocusIn}
			onfocusout={onFocusOut}
			onmouseenter={() => hold('pointer', true)}
			onmouseleave={() => hold('pointer', false)}
		>
			<span class="dot" aria-hidden="true"></span>
			<span class="msg">{s.undo.label}</span>
			<button
				class="undo"
				type="button"
				onclick={(e) => {
					leave(e.currentTarget.parentElement);
					void s.undoLast();
				}}>Undo</button
			>
			<button
				class="x"
				type="button"
				aria-label="Dismiss"
				onclick={(e) => {
					leave(e.currentTarget.parentElement);
					s.undo = null;
				}}>✕</button
			>
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

	.dot.info {
		background: var(--text-dim);
		box-shadow: none;
	}

	.notice .msg {
		white-space: normal;
		max-width: min(520px, calc(100vw - 110px));
	}

	.notice + .toast {
		margin-top: 8px;
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
