<script lang="ts">
	import type { VibeLook } from '$lib/vibes';

	// A line pictogram for each vibe: what kind of footage it's made for.
	let { icon, size = 26 }: { icon: VibeLook['icon']; size?: number } = $props();
</script>

<svg
	viewBox="0 0 24 24"
	width={size}
	height={size}
	fill="none"
	stroke="currentColor"
	stroke-width="1.7"
	stroke-linecap="round"
	stroke-linejoin="round"
	aria-hidden="true"
>
	{#if icon === 'kids'}
		<!-- a balloon on a string -->
		<path d="M12 3.5c-3 0-5 2.4-5 5.3 0 3.4 2.9 6.2 5 6.7 2.1-.5 5-3.3 5-6.7 0-2.9-2-5.3-5-5.3z" />
		<path d="M11.2 15.6l.8 1.2.8-1.2" />
		<path d="M12 16.8c-.8 1.4.8 2.2 0 3.7" />
		<path d="M9.6 7.4c.4-.9 1.2-1.5 2.1-1.6" />
	{:else if icon === 'family'}
		<!-- a house -->
		<path d="M4 11.2L12 4.5l8 6.7" />
		<path d="M6 9.8V19.5h12V9.8" />
		<path d="M10 19.5v-5h4v5" />
	{:else if icon === 'drone'}
		<!-- mountains under a sun -->
		<circle cx="17" cy="7" r="2.2" />
		<path d="M2.5 19.5l6-9 4 5.5 2.5-3 6.5 6.5z" />
	{:else if icon === 'action'}
		<!-- a lightning bolt -->
		<path d="M13.5 3L5.5 13.5h6L10 21l8.5-11h-6z" />
	{:else if icon === 'talk'}
		<!-- a speech bubble -->
		<path d="M4.5 6.5a2 2 0 0 1 2-2h11a2 2 0 0 1 2 2v7.5a2 2 0 0 1-2 2H11l-4.5 3.5V16h0a2 2 0 0 1-2-2z" />
		<path d="M8.5 9.3h7M8.5 12.3h4.5" />
	{:else}
		<!-- a heart -->
		<path d="M12 19.5s-7.5-4.4-7.5-9.6A4.1 4.1 0 0 1 12 7.6a4.1 4.1 0 0 1 7.5 2.3c0 5.2-7.5 9.6-7.5 9.6z" />
	{/if}
</svg>
