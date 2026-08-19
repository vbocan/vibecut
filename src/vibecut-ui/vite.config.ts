import { sveltekit } from '@sveltejs/kit/vite';
import { defineConfig } from 'vite';

// Dev-only proxy so the browser sees /api/* as same-origin — avoids
// needing CORS on vibecut-web. The intended dev workflow is this
// proxy, not permissive CORS on a server that binds 0.0.0.0.
// `ws: true` covers the render-progress WebSocket the same way.
export default defineConfig({
	plugins: [sveltekit()],
	server: {
		proxy: {
			'/api': {
				target: 'http://127.0.0.1:8090',
				changeOrigin: true,
				ws: true
			}
		}
	}
});
