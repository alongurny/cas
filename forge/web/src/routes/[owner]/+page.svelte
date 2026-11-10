<script lang="ts">
	import { page } from '$app/state';
	import { get } from '$lib/api';
	let projects = $state<any[]>([]);
	let account = $state<any>(null);
	$effect(() => {
		const o = page.params.owner;
		get(`users/${o}`).then((a) => (account = a)).catch(() => {});
		get('projects').then((ps) => (projects = ps.filter((p: any) => p.owner === o)));
	});
</script>

<h1>{page.params.owner} {#if account}<span class="muted">{account.kind}</span>{/if}</h1>
<ul class="list panel">
	{#each projects as p (p.name)}
		<li><a href="/{p.owner}/{p.name}">{p.name}</a> <span class="muted">{p.description ?? ''}</span></li>
	{:else}
		<li class="muted">No projects you can see.</li>
	{/each}
</ul>
