<script lang="ts">
	import { page } from '$app/state';
	import { get, qs, short } from '$lib/api';
	import Diff from '$lib/Diff.svelte';
	let base = $derived(`${page.params.owner}/${page.params.project}`);
	let sp = $derived(page.url.searchParams);
	let history = $derived(sp.get('history') ?? 'h/dev');
	let node = $derived(sp.get('node') ?? '');
	let step = $state<any>(null);
	let checks = $state<any[]>([]);
	let err = $state('');
	$effect(() => {
		step = null;
		get(`projects/${base}/step${qs({ history, node })}`)
			.then((s) => {
				step = s;
				get(`projects/${base}/checks${qs({ history, node })}`).then((c) => (checks = c));
			})
			.catch((e) => (err = e.message));
	});
</script>

{#if err}<p class="error">{err}</p>{/if}
{#if step}
	<div class="panel">
		<div class="row">
			<span class="mono">{short(step.entry.id)}</span>
			{#each step.entry.tags as t (t)}<span class="tag">{t}</span>{/each}
			<span class="grow"></span>
			{#each step.entry.after as a (a.id)}
				<span class="muted">after <a class="mono" href="/{base}/step{qs({ history, node: a.id })}">{short(a.id)}</a>{a.compatible ? ' (compatible)' : ''}</span>
			{/each}
			<a href="/{base}/tree{qs({ history, node: step.entry.id })}">files</a>
		</div>
		{#if step.changelog}<pre>{step.changelog}</pre>{/if}
		{#if step.note}<pre>{step.note}</pre>{/if}
		{#if step.entry.doc}<pre class="muted">{step.entry.doc}</pre>{/if}
		{#each checks as c (c.name)}<span class="tag {c.status}">{c.name}: {c.status}</span> {/each}
	</div>
	<Diff lines={step.lines} />
{/if}
