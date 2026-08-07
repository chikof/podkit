import tailwindcss from '@tailwindcss/vite';
import adapter from '@sveltejs/adapter-static';
import { sveltekit } from '@sveltejs/kit/vite';
import { defineConfig } from 'vite';

export default defineConfig({
	plugins: [
		tailwindcss(),
		sveltekit({
			compilerOptions: {
				// Force runes mode for the project, except for libraries. Can be removed in svelte 6.
				runes: ({ filename }) =>
					filename.split(/[/\\]/).includes('node_modules') ? undefined : true
			},

			// Pure client-rendered SPA: every route needs a bearer token that
			// only ever lives in sessionStorage, so there is nothing useful
			// for SSR to do. `fallback: 'index.html'` lets any deep link
			// (e.g. /t/123/projects/456) resolve client-side after a
			// full-page load or on a static host with no server-side routing.
			adapter: adapter({
				fallback: 'index.html',
				pages: 'build',
				assets: 'build'
			})
		})
	]
});
