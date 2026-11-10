<script lang="ts">
	import { page } from '$app/state';
	import { get, qs, short } from '$lib/api';
	let base = $derived(`${page.params.owner}/${page.params.project}`);
	let sp = $derived(page.url.searchParams);
	let history = $derived(sp.get('history') ?? 'h/dev');
	let path = $derived(sp.get('path') ?? '');
	let skip = $derived(Number(sp.get('skip') ?? 0));
	let log = $state<any>(null);
	let err = $state('');
	$effect(() => {
		log = null;
		get(`projects/${base}/log${qs({ history, path, skip, limit: 50 })}`).then((l) => (log = l)).catch((e) => (err = e.message));
	});
</script>

<p class="mono">{history}{path ? ' — ' + path : ''}</p>
{#if err}<p class="error">{err}</p>{/if}
{#if log}
	<ul class="list panel">
		{#each log.entries as e (e.id)}
			<li>
				<div class="row">
					<a class="mono" href="/{base}/step{qs({ history, node: e.id })}">{short(e.id)}</a>
					{#each e.tags as t (t)}<span class="tag">{t}</span>{/each}
					{#if e.after.length > 1}<span class="tag">merge</span>{/if}
					<span class="grow">{e.line || '(no notes)'}</span>
					<a href="/{base}/tree{qs({ history, node: e.id })}">files</a>
				</div>
			</li>
		{/each}
	</ul>
	<div class="row">
		{#if skip > 0}<a href="/{base}/log{qs({ history, path, skip: Math.max(0, skip - 50) })}">newer</a>{/if}
		<span class="muted">{skip + 1}–{Math.min(skip + 50, log.total)} of {log.total}</span>
		{#if skip + 50 < log.total}<a href="/{base}/log{qs({ history, path, skip: skip + 50 })}">older</a>{/if}
	</div>
{/if}
