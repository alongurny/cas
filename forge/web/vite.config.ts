import { sveltekit } from '@sveltejs/kit/vite';
import { defineConfig } from 'vite';

declare const process: { env: Record<string, string | undefined> };

// `npm run dev` proxies the API to a forge (FORGE, default https://localhost:8443).
export default defineConfig({
	plugins: [sveltekit()],
	server: {
		proxy: { '/api': { target: process.env.FORGE ?? 'https://localhost:8443', secure: false } }
	}
});
