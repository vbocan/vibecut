<script lang="ts">
	import { onDestroy } from 'svelte';
	import { appState as s } from '$lib/state.svelte';

	// Both toasts count down while nobody is on them, and wait while one
	// has the pointer or the keyboard focus (WCAG 2.2.1); the countdown
	// starts over when both leave. A toast that goes away under the
	// keyboard focus (Undo, Dismiss) hands it back to where it came from
	// instead of dropping it on the page.
	function countdown(ms: number, current: () => unknown, expire: (item: unknown) => void) {
		const held = { focus: false, pointer: false };
		let timer: ReturnType<typeof setTimeout> | null = null;
		let returnTo: HTMLElement | null = null;
		function arm() {
			const item = current();
			if (timer) clearTimeout(timer);
			timer = null;
			if (!item || held.focus || held.pointer) return;
			timer = setTimeout(() => {
				if (current() === item) expire(item);
			}, ms);
		}
		function hold(kind: keyof typeof held, on: boolean) {
			held[kind] = on;
			arm();
		}
		return {
			arm,
			/** A new item: nobody is on it yet. */
			restart() {
				held.focus = held.pointer = false;
				arm();
			},
			focusin(e: FocusEvent & { currentTarget: HTMLElement }) {
				const from = e.relatedTarget as HTMLElement | null;
				if (!e.currentTarget.contains(from)) returnTo = from;
				hold('focus', true);
			},
			focusout(e: FocusEvent & { currentTarget: HTMLElement }) {
				if (!e.currentTarget.contains(e.relatedTarget as Node | null)) hold('focus', false);
			},
			enter: () => hold('pointer', true),
			leave: () => hold('pointer', false),
			/** The toast is about to go: give the focus back first. */
			close(toast: HTMLElement | null) {
				if (toast?.contains(document.activeElement) && returnTo?.isConnected) returnTo.focus();
				held.focus = held.pointer = false;
				returnTo = null;
			},
			stop() {
				if (timer) clearTimeout(timer);
			}
		};
	}

	// Shown once the re-render for the last change has finished; one Undo
	// restores the settings and the previous version instantly.
	let shownFor = $state<object | null>(null);
	const visible = $derived(!!s.undo && s.renderMode === 'done' && shownFor === s.undo);
	const undoClock = countdown(
		12000,
		() => s.undo,
		(u) => {
			if (s.undo === u) s.undo = null;
		}
	);
	$effect(() => {
		const u = s.undo;
		if (u && s.renderMode === 'done' && shownFor !== u) {
			shownFor = u;
			undoClock.restart();
		}
	});

	// Short notices (a stopped render) fade on their own after a few
	// seconds — unless someone is reading them.
	let noticeEl: HTMLDivElement | undefined = $state();
	const noticeClock = countdown(
		5000,
		() => s.notice,
		(n) => {
			if (s.notice !== n) return;
			noticeClock.close(noticeEl ?? null);
			s.notice = null;
		}
	);
	let noticeFor: object | null = null;
	$effect(() => {
		const n = s.notice;
		if (n === noticeFor) return;
		// A notice replacing one that is being read keeps it held.
		if (n && noticeFor) noticeClock.arm();
		else if (n) noticeClock.restart();
		else noticeClock.stop();
		noticeFor = n;
	});

	onDestroy(() => {
		undoClock.stop();
		noticeClock.stop();
	});
</script>

<!-- One status line each for screen readers (the text once, not the
     buttons too); the toasts themselves aren't second live regions. -->
<p class="sr-only" role="status">{s.notice ? s.notice.text : ''}</p>
<p class="sr-only" role="status">{visible && s.undo ? `${s.undo.label}. You can undo this.` : ''}</p>
<div class="toasts">
	{#if s.notice}
		<div
			class="toast notice"
			role="group"
			aria-label="Notice"
			bind:this={noticeEl}
			onfocusin={noticeClock.focusin}
			onfocusout={noticeClock.focusout}
			onmouseenter={noticeClock.enter}
			onmouseleave={noticeClock.leave}
		>
			<span class="dot info" aria-hidden="true"></span>
			<span class="msg">{s.notice.text}</span>
			<button
				class="x"
				type="button"
				aria-label="Dismiss notice"
				onclick={(e) => {
					noticeClock.close(e.currentTarget.parentElement);
					s.notice = null;
				}}>✕</button
			>
		</div>
	{/if}
	{#if visible && s.undo}
		<div
			class="toast"
			role="group"
			aria-label="Last change"
			onfocusin={undoClock.focusin}
			onfocusout={undoClock.focusout}
			onmouseenter={undoClock.enter}
			onmouseleave={undoClock.leave}
		>
			<span class="dot" aria-hidden="true"></span>
			<span class="msg">{s.undo.label}</span>
			<button
				class="undo"
				type="button"
				onclick={(e) => {
					undoClock.close(e.currentTarget.parentElement);
					void s.undoLast();
				}}>Undo</button
			>
			<button
				class="x"
				type="button"
				aria-label="Dismiss"
				onclick={(e) => {
					undoClock.close(e.currentTarget.parentElement);
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
