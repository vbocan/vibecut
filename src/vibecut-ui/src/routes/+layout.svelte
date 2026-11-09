<script lang="ts">
	// Fonts are self-hosted (bundled by Vite from @fontsource), so a home
	// server with no internet keeps the look and the app makes no
	// third-party requests.
	import '@fontsource-variable/bricolage-grotesque/opsz.css';
	import '@fontsource/instrument-serif/400.css';
	import '@fontsource/instrument-serif/400-italic.css';
	import '@fontsource/ibm-plex-mono/400.css';
	import '@fontsource/ibm-plex-mono/500.css';
	import '@fontsource/ibm-plex-mono/600.css';
	import '../app.css';
	let { children } = $props();
</script>

<a class="skip" href="#main">Skip to content</a>
{@render children()}

<style>
	.skip {
		position: absolute;
		left: 12px;
		top: -60px;
		z-index: 100;
		padding: 8px 14px;
		border-radius: 999px;
		background: var(--text);
		color: var(--ink);
		font-weight: 700;
		text-decoration: none;
	}

	.skip:focus {
		top: 12px;
	}
</style>
