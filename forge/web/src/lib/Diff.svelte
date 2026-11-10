<script lang="ts">
	// A diff (spec/formats/diff.md) with a note on each line, as on the
	// phone: the note under its line, not in a gutter. A line of a file can
	// be picked, for a review thread.
	type Line = { text: string; note?: string };
	type Pick = { path: string; side: 'old' | 'new'; line: number; text: string };
	let {
		lines,
		threads = [],
		onpick
	}: { lines: Line[]; threads?: { path: string; side: string; line_now: number; id: string }[]; onpick?: (p: Pick) => void } = $props();

	type Row = { kind: string; text: string; note: string; path: string; old?: number; new?: number };
	let rows = $derived.by(() => {
		const out: Row[] = [];
		let path = '';
		let o = 0;
		let n = 0;
		for (const l of lines) {
			const t = l.text;
			const note = l.note ?? '';
			if (t.startsWith('diff ')) continue;
			if (t.startsWith('path ')) {
				path = t.slice(5);
				out.push({ kind: 'path', text: path, note, path });
			} else if (t.startsWith('old ') || t.startsWith('new ')) {
				continue;
			} else if (t === 'binary') {
				out.push({ kind: 'meta', text: 'binary file', note, path });
			} else if (t.startsWith('@@')) {
				const m = t.match(/-(\d+),(\d+) \+(\d+),(\d+)/);
				if (m) {
					o = Number(m[2]) === 0 ? Number(m[1]) : Number(m[1]) - 1;
					n = Number(m[4]) === 0 ? Number(m[3]) : Number(m[3]) - 1;
				}
				out.push({ kind: 'hunk', text: t, note, path });
			} else if (t === '\\') {
				continue;
			} else if (t.startsWith('-')) {
				o++;
				out.push({ kind: 'del', text: t.slice(1), note, path, old: o });
			} else if (t.startsWith('+')) {
				n++;
				out.push({ kind: 'add', text: t.slice(1), note, path, new: n });
			} else {
				o++;
				n++;
				out.push({ kind: 'ctx', text: t.slice(1), note, path, old: o, new: n });
			}
		}
		return out;
	});

	function threadsAt(r: Row) {
		return threads.filter(
			(t) => t.path === r.path && ((t.side === 'new' && t.line_now === r.new) || (t.side === 'old' && r.kind === 'del' && t.line_now === r.old))
		);
	}

	function pick(r: Row) {
		if (!onpick) return;
		if (r.kind === 'del') onpick({ path: r.path, side: 'old', line: r.old!, text: r.text });
		else if (r.new) onpick({ path: r.path, side: 'new', line: r.new, text: r.text });
	}
</script>

<div class="diff">
	{#each rows as r, i (i)}
		{#if r.kind === 'path'}
			<div class="path mono">{r.text}</div>
			{#if r.note}<div class="note">{r.note}</div>{/if}
		{:else if r.kind === 'hunk' || r.kind === 'meta'}
			<div class="hunk mono">{r.text}</div>
		{:else}
			<button type="button" class="line {r.kind}" class:pickable={!!onpick} onclick={() => pick(r)}>
				<span class="num">{r.old ?? ''}</span><span class="num">{r.new ?? ''}</span><span class="sign">{r.kind === 'add' ? '+' : r.kind === 'del' ? '-' : ' '}</span><code>{r.text}</code>
			</button>
			{#if r.note}<div class="note">{r.note}</div>{/if}
			{#each threadsAt(r) as t (t.id)}
				<div class="thread-mark"><a href="#thread-{t.id}">thread {t.id}</a></div>
			{/each}
		{/if}
	{/each}
</div>

<style>
	.diff { border: 1px solid var(--line); border-radius: 8px; overflow: hidden; background: var(--panel); }
	.path { padding: 6px 10px; background: var(--bg); border-top: 1px solid var(--line); font-weight: 600; }
	.path:first-child { border-top: none; }
	.hunk { padding: 2px 10px; color: var(--muted); background: var(--bg); }
	.line { display: flex; width: 100%; text-align: left; border: none; border-radius: 0; padding: 0 6px; background: transparent; font: inherit; }
	.line.pickable:hover { outline: 1px solid var(--accent); }
	.line code { white-space: pre-wrap; word-break: break-word; flex: 1; }
	.add { background: var(--add); color: var(--add-text); }
	.del { background: var(--del); color: var(--del-text); }
	.num { width: 3.2em; flex: none; text-align: right; padding-right: 6px; color: var(--muted); font-family: var(--mono); font-size: 12px; user-select: none; }
	.sign { width: 1.2em; flex: none; font-family: var(--mono); }
	.note { background: var(--note); color: var(--note-text); padding: 2px 10px 2px 3.8em; font-size: 13px; }
	.thread-mark { padding: 0 10px 0 3.8em; font-size: 12px; }
	@media (max-width: 600px) { .num { display: none; } .note, .thread-mark { padding-left: 1.6em; } }
</style>
