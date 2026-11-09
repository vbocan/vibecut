<script lang="ts">
	import { onMount } from 'svelte';
	import { appState as s } from '$lib/state.svelte';
	import Topbar from '$lib/components/Topbar.svelte';
	import CreateScreen from '$lib/components/CreateScreen.svelte';
	import CuttingRoom from '$lib/components/CuttingRoom.svelte';
	import StudioScreen from '$lib/components/StudioScreen.svelte';
	import ProDrawer from '$lib/components/ProDrawer.svelte';
	import UndoToast from '$lib/components/UndoToast.svelte';
	import { prefersReducedMotion } from '$lib/motion';
	import { focusTransient } from '$lib/focus';

	let proOpen = $state(false);
	// A render that finishes while the tab is in the background flags the
	// tab title, so people who wandered off see it's ready.
	let unseenReady = $state(false);
	let lastStep = s.makeStep;
	$effect(() => {
		const step = s.makeStep;
		if (step === 'done' && lastStep !== 'done' && document.hidden) unseenReady = true;
		lastStep = step;
	});

	onMount(() => {
		void s.loadHealth();
		void s.loadVibes();
		void s.loadFootage();
		void s.loadMusic();
		void s.loadSessions();
		void s.restore();
		const seen = () => {
			if (!document.hidden) unseenReady = false;
		};
		document.addEventListener('visibilitychange', seen);
		return () => document.removeEventListener('visibilitychange', seen);
	});

	// Three screens, chosen by where the flow is:
	//  create   — pick footage, vibe, song
	//  cutting  — the first render of a project (full-screen progress)
	//  studio   — a finished video; later re-renders overlay the player
	const busy = $derived(
		s.makeStep === 'scanning' ||
			s.makeStep === 'planning' ||
			s.makeStep === 'scoring' ||
			s.renderMode === 'queued' ||
			s.renderMode === 'running'
	);
	const hasVideo = $derived(
		s.proposeResult !== null && (s.renderHistory.length > 0 || (s.renderMode === 'done' && !!s.outputPath))
	);
	const screen = $derived<'create' | 'cutting' | 'studio'>(
		hasVideo ? 'studio' : busy || s.makeStep === 'failed' ? 'cutting' : 'create'
	);

	// Changing screens removes the control that had the keyboard focus
	// ("Make my video", the cutting room's buttons): start the new
	// screen's reading order at its top instead of dropping focus on the
	// page. Only once someone has used the page, so restoring a session
	// on load doesn't move anything. <main> is focusable only for that
	// visit: a lasting tabindex would restart the Tab order at its top
	// after every click on its text.
	let mainEl: HTMLElement | undefined = $state();
	let interacted = false;
	let lastScreen: string | null = null;
	$effect(() => {
		const now = screen;
		const at = document.activeElement;
		if (interacted && lastScreen !== null && lastScreen !== now && (!at || at === document.body) && mainEl)
			focusTransient(mainEl);
		lastScreen = now;
	});

	// The whole UI breathes at the chosen song's tempo.
	$effect(() => {
		const beat = s.effectiveTrack?.beat_interval_secs;
		document.documentElement.style.setProperty('--beat', `${beat && beat > 0.2 ? beat : 0.5}s`);
	});

	// Keep the server's "Picked for you" in step with the vibe, length and
	// seed (it reads them before its first await, so they're tracked).
	$effect(() => {
		void s.refreshAutoPick();
	});

	function newVideo() {
		if (s.renderMode === 'queued' || s.renderMode === 'running') return;
		if (
			s.renderHistory.length > 0 &&
			!confirm('Start a new video? This one stays saved in your output folder, but its versions leave the Studio.')
		)
			return;
		s.startOver();
		proOpen = false;
		window.scrollTo({ top: 0, behavior: prefersReducedMotion() ? 'auto' : 'smooth' });
	}

	function backFromFailure() {
		s.makeStep = 'idle';
		s.makeError = null;
	}
</script>

<svelte:head>
	<title
		>{unseenReady
			? '● Ready · Vibecut'
			: s.makeStep === 'failed'
				? 'Render stopped · Vibecut'
				: screen === 'cutting'
				? `${Math.round((s.renderProgress?.overall ?? 0) * 100)}% · Cutting… · Vibecut`
				: screen === 'studio'
					? 'Studio · Vibecut'
					: 'Vibecut'}</title
	>
	<meta name="description" content="Turn a folder of GoPro or DJI footage into a beat-locked highlight video." />
</svelte:head>

<Topbar onOpenPro={() => (proOpen = true)} onNew={newVideo} />

<svelte:window onkeydown={() => (interacted = true)} onpointerdown={() => (interacted = true)} />

<main id="main" bind:this={mainEl}>
	{#key screen}
		<div class="screen">
			{#if screen === 'create'}
				<CreateScreen />
			{:else if screen === 'cutting'}
				<CuttingRoom onBack={backFromFailure} />
			{:else}
				<StudioScreen onOpenPro={() => (proOpen = true)} />
			{/if}
		</div>
	{/key}
</main>

<p class="sr-only" role="status">{screen === 'studio' && s.renderMode === 'done' ? 'Your video is ready.' : ''}</p>

<ProDrawer bind:open={proOpen} />
<UndoToast />

<style>
	/* Focused only as a reading position (skip link, screen changes). */
	main:focus {
		outline: none;
	}

	.screen {
		animation: enter 700ms var(--ease-out) both;
	}

	@keyframes enter {
		from {
			opacity: 0;
			transform: translateY(10px) scale(0.995);
			filter: blur(6px);
		}
	}
</style>
