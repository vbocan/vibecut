import adapter from '@sveltejs/adapter-static';
import { vitePreprocess } from '@sveltejs/vite-plugin-svelte';

/** @type {import('@sveltejs/kit').Config} */
const config = {
	preprocess: vitePreprocess(),
	kit: {
		// SPA build: this app is a pure client that talks to
		// vibecut-web's REST API over fetch/WebSocket — there's no
		// server-rendered data to prerender against.
		adapter: adapter({
			fallback: 'index.html'
		})
	}
};

export default config;
