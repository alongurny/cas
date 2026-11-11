// Makes src/routes, which SvelteKit reads, from src/pages. SvelteKit wants
// names such as +page.svelte and [owner], which a cas directory does not
// hold, so the pages are kept under names that fit: page.svelte and
// layout.* stand for +page.svelte and +layout.*, and a directory _owner for
// [owner]. Each file of src/routes is a link to its page, so an edit shows
// at once in `npm run dev`; a new or renamed page needs this run again.
import { mkdirSync, readdirSync, rmSync, symlinkSync } from 'node:fs';
import { join, relative } from 'node:path';

function link(from, to) {
	mkdirSync(to, { recursive: true });
	for (const e of readdirSync(from, { withFileTypes: true })) {
		if (e.isDirectory()) {
			link(join(from, e.name), join(to, e.name.startsWith('_') ? `[${e.name.slice(1)}]` : e.name));
		} else {
			const name = /^(page|layout)\./.test(e.name) ? `+${e.name}` : e.name;
			symlinkSync(relative(to, join(from, e.name)), join(to, name));
		}
	}
}

rmSync('src/routes', { recursive: true, force: true });
link('src/pages', 'src/routes');
