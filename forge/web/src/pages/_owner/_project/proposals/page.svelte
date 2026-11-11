<script lang="ts">
	import { page } from '$app/state';
	import { get, api, qs, when } from '#lib/api.ts';
	import { goto } from '$app/navigation';
	let base = $derived(`${page.params.owner}/${page.params.project}`);
	let state_ = $derived(page.url.searchParams.get('state') ?? 'open');
	let list = $state<any[]>([]);
	let histories = $state<any[]>([]);
	let title = $state('');
	let source = $state('');
	let target = $state('h/dev');
	let err = $state('');
	$effect(() => {
		get(`projects/${base}/proposals${qs({ state: state_ === 'all' ? '' : state_ })}`).then((l) => (list = l));
		get(`projects/${base}/histories`).then((h) => (histories = h)).catch(() => {});
	});
	async function open(e: Event) {
		e.preventDefault();
		err = '';
		try {
			const r = await api('POST', `projects/${base}/proposals`, { title, source, target });
			goto(`/${base}/proposals/${r.number}`);
		} catch (x: any) {
			err = x.message;
		}
	}
</script>

<div class="row">
	{#each ['open', 'merged', 'closed', 'all'] as s (s)}
		<a href="?state={s}" class="tag" class:open={s === state_}>{s}</a>
	{/each}
</div>
<ul class="list panel">
	{#each list as p (p.number)}
		<li class="row">
			<span class="tag {p.state}">{p.state}</span>
			<a class="grow" href="/{base}/proposals/{p.number}">#{p.number} {p.title}</a>
			<span class="muted mono">{p.source} → {p.target}</span>
			<span class="muted">{p.author}, {when(p.updated)}</span>
		</li>
	{:else}
		<li class="muted">No proposals.</li>
	{/each}
</ul>

<h2>Propose</h2>
<form class="panel row" onsubmit={open}>
	<input class="grow" bind:value={title} placeholder="title (one line; the steps' notes say the rest)" required />
	<select bind:value={source} required>
		<option value="" disabled>from your history</option>
		{#each histories.filter((h) => h.name.startsWith('u/')) as h (h.name)}<option>{h.name}</option>{/each}
	</select>
	<select bind:value={target}>
		{#each histories.filter((h) => h.name.startsWith('h/')) as h (h.name)}<option>{h.name}</option>{/each}
		{#if !histories.some((h) => h.name === 'h/dev')}<option>h/dev</option>{/if}
	</select>
	<button class="primary">Open</button>
	{#if err}<span class="error">{err}</span>{/if}
</form>
