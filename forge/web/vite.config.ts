import adapter from '@sveltejs/adapter-static';
import { sveltekit } from '@sveltejs/kit/vite';
import { vitePreprocess } from '@sveltejs/vite-plugin-svelte';
import { defineConfig } from 'vite';

declare const process: { env: Record<string, string | undefined> };

// `npm run dev` proxies the API to a forge (FORGE, default https://localhost:8443).
export default defineConfig({
	// The forge serves the build: index.html for every page path.
	plugins: [sveltekit({ preprocess: vitePreprocess(), adapter: adapter({ fallback: 'index.html' }) })],
	server: {
		proxy: { '/api': { target: process.env.FORGE ?? 'https://localhost:8443', secure: false } }
	}
});
