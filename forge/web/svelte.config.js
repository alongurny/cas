import adapter from '@sveltejs/adapter-static';
import { vitePreprocess } from '@sveltejs/vite-plugin-svelte';

/** The forge serves the build: index.html for every page path. */
export default {
	preprocess: vitePreprocess(),
	kit: {
		adapter: adapter({ fallback: 'index.html' })
	}
};
