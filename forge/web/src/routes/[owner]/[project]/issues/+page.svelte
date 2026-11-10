<script lang="ts">
	import { page } from '$app/state';
	import { get, qs, when } from '$lib/api';
	let base = $derived(`${page.params.owner}/${page.params.project}`);
	let sp = $derived(page.url.searchParams);
	let state_ = $derived(sp.get('state') ?? 'open');
	let label = $derived(sp.get('label') ?? '');
	let list = $state<any[]>([]);
	$effect(() => {
		get(`projects/${base}/issues${qs({ state: state_ === 'all' ? '' : state_, label })}`).then((l) => (list = l));
	});
</script>

<div class="row">
	{#each ['open', 'closed', 'all'] as s (s)}<a href={qs({ state: s, label })} class="tag" class:open={s === state_}>{s}</a>{/each}
	{#if label}<span class="tag">label {label} <a href={qs({ state: state_ })}>×</a></span>{/if}
	<span class="grow"></span>
	<a href="/{base}/issues/new">New issue</a>
</div>
<ul class="list panel">
	{#each list as i (i.number)}
		<li class="row">
			<span class="tag {i.state}">{i.state}</span>
			<a class="grow" href="/{base}/issues/{i.number}">#{i.number} {i.title}</a>
			{#each i.labels as l (l)}<a class="tag" href={qs({ state: state_, label: l })}>{l}</a>{/each}
			<span class="muted">{i.author}, {when(i.updated)}</span>
		</li>
	{:else}
		<li class="muted">No issues.</li>
	{/each}
</ul>
