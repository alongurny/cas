<script lang="ts">
	import { get, api, user } from '$lib/api';
	import { goto } from '$app/navigation';
	let projects = $state<any[]>([]);
	let me = $state<any>(null);
	let name = $state('');
	let description = $state('');
	let pub = $state(false);
	let err = $state('');
	$effect(() => {
		get('projects').then((p) => (projects = p));
		user().then((u) => (me = u));
	});
	async function create(e: Event) {
		e.preventDefault();
		err = '';
		try {
			const r = await api('POST', 'projects', { name, description, public: pub });
			goto(`/${r.project}`);
		} catch (x: any) {
			err = x.message;
		}
	}
</script>

<h1>Projects</h1>
<ul class="list panel">
	{#each projects as p (p.owner + '/' + p.name)}
		<li>
			<a href="/{p.owner}/{p.name}"><strong>{p.owner}/{p.name}</strong></a>
			<span class="tag">{p.visibility}</span>
			<span class="muted">{p.description ?? ''}</span>
		</li>
	{:else}
		<li class="muted">No projects you can see{me ? '' : ' (sign in to see more)'}.</li>
	{/each}
</ul>

{#if me}
	<h2>New project</h2>
	<form class="panel row" onsubmit={create}>
		<span class="muted">{me.name}/</span>
		<input bind:value={name} placeholder="name" required />
		<input class="grow" bind:value={description} placeholder="description" />
		<label><input type="checkbox" bind:checked={pub} /> public</label>
		<button class="primary">Create</button>
		{#if err}<span class="error">{err}</span>{/if}
	</form>
{/if}
