import tailwindcss from '@tailwindcss/vite';
import { sveltekit } from '@sveltejs/kit/vite';
import { defineConfig } from 'vite';

export default defineConfig({
	plugins: [tailwindcss(), sveltekit()],
	build: {
		target: 'es2022',
		minify: 'oxc',
		sourcemap: false,
		rollupOptions: {
			output: {
				manualChunks: (id) => {
					if (id.includes('node_modules/d3')) return 'd3';
					if (id.includes('node_modules/@lucide/svelte')) return 'lucide';
				}
			}
		}
	}
});
