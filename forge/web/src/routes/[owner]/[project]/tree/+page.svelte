<script lang="ts">
	import { page } from '$app/state';
	import { get, qs, short } from '#lib/api.ts';
	let base = $derived(`${page.params.owner}/${page.params.project}`);
	let sp = $derived(page.url.searchParams);
	let history = $derived(sp.get('history') ?? 'h/dev');
	let node = $derived(sp.get('node') ?? '');
	let path = $derived(sp.get('path') ?? '');
	let showBlame = $derived(sp.get('blame') === '1');
	let tree = $state<any>(null);
	let file = $state<any>(null);
	let blame = $state<any>(null);
	let err = $state('');
	$effect(() => {
		err = '';
		tree = file = blame = null;
		const q = { history, node, path };
		get(`projects/${base}/tree${qs(q)}`)
			.then(async (t) => {
				tree = t;
				if (t.kind === 'file') {
					file = await get(`projects/${base}/file${qs(q)}`);
					if (showBlame) blame = await get(`projects/${base}/blame${qs(q)}`);
				}
			})
			.catch((e) => (err = e.message));
	});
	let crumbs = $derived(path ? path.split('/') : []);
	function link(p: string, extra: Record<string, string> = {}) {
		return `/${base}/tree${qs({ history, node, path: p, ...extra })}`;
	}
</script>

<div class="row">
	<span class="mono">{history}{node ? ' @ ' + short(node) : ''}</span>
	<span class="muted">/</span>
	<a href={link('')}>root</a>
	{#each crumbs as c, i (i)}
		<span class="muted">/</span><a href={link(crumbs.slice(0, i + 1).join('/'))}>{c}</a>
	{/each}
	<span class="grow"></span>
	{#if path}<a href="/{base}/log{qs({ history, path })}">history of this path</a>{/if}
</div>
{#if err}<p class="error">{err}</p>{/if}
{#if tree?.kind === 'dir'}
	<ul class="list panel">
		{#each tree.entries as e (e.name)}
			<li class="row">
				<span>{e.kind === 'dir' ? '📁' : '📄'}</span>
				<a class="grow" href={link(path ? `${path}/${e.name}` : e.name)}>{e.name}</a>
				<span class="muted">{e.kind === 'file' ? e.format : ''}</span>
			</li>
		{/each}
	</ul>
{:else if file}
	<div class="row">
		<span class="muted">{file.format} · {file.size} bytes</span>
		<span class="grow"></span>
		<a href={link(path, showBlame ? {} : { blame: '1' })}>{showBlame ? 'hide' : 'show'} who changed each line</a>
		<a href="/api/v1/projects/{base}/raw{qs({ history, node, path })}">raw</a>
	</div>
	{#if file.text === null}
		<p class="muted">Binary or too large to show.</p>
	{:else if blame}
		<div class="panel code">
			{#each blame.lines as l, i (i)}
				<div class="bl">
					<a class="who mono" href="/{base}/step{qs({ history, node: l.node })}" title={l.line ?? ''}>{short(l.node)}</a>
					<span class="n mono">{i + 1}</span><code>{l.text}</code>
				</div>
				{#if l.line && (i === 0 || blame.lines[i - 1].node !== l.node)}<div class="why">{l.line}</div>{/if}
			{/each}
		</div>
	{:else}
		<div class="panel code">
			{#each file.text.replace(/\n$/, '').split('\n') as l, i (i)}
				<div class="bl"><span class="n mono">{i + 1}</span><code>{l}</code></div>
			{/each}
		</div>
	{/if}
{/if}

<style>
	.code { padding: 6px 0; overflow-x: auto; }
	.bl { display: flex; gap: 6px; padding: 0 8px; }
	.bl code { white-space: pre-wrap; word-break: break-word; }
	.n { width: 3em; flex: none; text-align: right; color: var(--muted); user-select: none; }
	.who { width: 7em; flex: none; font-size: 12px; }
	.why { background: var(--note); color: var(--note-text); font-size: 12px; padding: 0 8px 0 10em; }
	@media (max-width: 600px) { .why { padding-left: 8px; } }
</style>
