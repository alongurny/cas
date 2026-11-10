<script lang="ts">
	import { page } from '$app/state';
	import { get, api } from '$lib/api';
	let base = $derived(`${page.params.owner}/${page.params.project}`);
	let p = $state<any>(null);
	let err = $state('');
	let member = $state('');
	let role = $state('write');
	let gateHistory = $state('h/dev');
	let approvals = $state(0);
	let checks = $state('');
	let resolved = $state(false);
	let hookUrl = $state('');
	let hookSecret = $state('');
	let runner = $state('');
	let label = $state('');
	let labelSet = $state('');
	async function load() {
		p = await get(`projects/${base}`).catch((e) => ((err = e.message), null));
	}
	$effect(() => {
		load();
	});
	async function act(method: string, path: string, body?: unknown) {
		err = '';
		try {
			await api(method, `projects/${base}${path}`, body);
			await load();
		} catch (x: any) {
			err = x.message;
		}
	}
	const keys = (o: any) => Object.keys(o ?? {});
</script>

{#if err}<p class="error">{err}</p>{/if}
{#if p}
	{#if p.role !== 'admin'}<p class="muted">Only admins change settings; you are {p.role}.</p>{/if}
	<h2>Project</h2>
	<form class="panel row" onsubmit={(e) => { e.preventDefault(); act('PATCH', '', { description: p.description ?? '', visibility: p.visibility }); }}>
		<input class="grow" bind:value={p.description} placeholder="description" />
		<select bind:value={p.visibility}><option>private</option><option>public</option></select>
		<button>Save</button>
	</form>

	<h2>Members</h2>
	<ul class="list panel">
		{#each keys(p.members) as m (m)}
			<li class="row"><span class="grow">{m}</span><span class="tag">{p.members[m]}</span><button onclick={() => act('DELETE', `/members/${m}`)}>Remove</button></li>
		{/each}
	</ul>
	<form class="row" onsubmit={(e) => { e.preventDefault(); act('PUT', `/members/${member}`, { role }); }}>
		<input bind:value={member} placeholder="user or organization" required />
		<select bind:value={role}><option>read</option><option>write</option><option>maintain</option><option>admin</option></select>
		<button>Add</button>
	</form>

	<h2>Protected histories</h2>
	<p class="muted">They move only through merges and releases here, never by a push.</p>
	<ul class="list panel">
		{#each keys(p.protected) as h (h)}
			<li class="row"><span class="grow mono">{h}</span><button onclick={() => act('PUT', `/protected/${h}`, { on: false })}>Unprotect</button></li>
		{/each}
	</ul>

	<h2>Gates</h2>
	<ul class="list panel">
		{#each keys(p.gates) as h (h)}
			<li class="mono">{h}: {p.gates[h].approvals ?? 0} approvals; checks {keys(p.gates[h].checks).join(', ') || 'none'}{p.gates[h].resolved ? '; threads resolved' : ''}</li>
		{/each}
	</ul>
	<form class="row" onsubmit={(e) => { e.preventDefault(); act('PUT', `/gates/${gateHistory}`, { approvals, checks: checks.split(',').map((s) => s.trim()).filter(Boolean), resolved }); }}>
		<input bind:value={gateHistory} placeholder="h/dev" />
		<label>approvals <input type="number" min="0" bind:value={approvals} style="width: 4em" /></label>
		<input bind:value={checks} placeholder="required checks, comma separated" />
		<label><input type="checkbox" bind:checked={resolved} /> threads resolved</label>
		<button>Set</button>
	</form>

	<h2>Labels</h2>
	<div class="row">
		{#each keys(p.labels) as l (l)}
			<span class="tag" style="border-color: {p.labels[l].color}">{l}{p.labels[l].set ? ' · ' + p.labels[l].set : ''} <button class="x" onclick={() => act('DELETE', `/labels/${encodeURIComponent(l)}`)}>×</button></span>
		{/each}
	</div>
	<form class="row" onsubmit={(e) => { e.preventDefault(); act('PUT', `/labels/${encodeURIComponent(label)}`, { set: labelSet }); label = ''; }}>
		<input bind:value={label} placeholder="label" required />
		<input bind:value={labelSet} placeholder="exclusive set (optional)" />
		<button>Add</button>
	</form>

	<h2>Webhooks</h2>
	<ul class="list panel">
		{#each keys(p.webhooks) as id (id)}
			<li class="row"><span class="grow mono">{p.webhooks[id].url}</span><span class="muted">{p.webhooks[id].events}</span><button onclick={() => act('DELETE', `/webhooks/${id}`)}>Remove</button></li>
		{/each}
	</ul>
	<form class="row" onsubmit={(e) => { e.preventDefault(); act('POST', '/webhooks', { url: hookUrl, secret: hookSecret }); hookUrl = hookSecret = ''; }}>
		<input class="grow" bind:value={hookUrl} placeholder="https://…" required />
		<input type="password" bind:value={hookSecret} placeholder="secret (signs each delivery)" />
		<button>Add</button>
	</form>

	<h2>Check runners</h2>
	<ul class="list panel">
		{#each keys(p.runners) as r (r)}
			<li class="row"><span class="grow">{r}</span><button onclick={() => act('DELETE', `/runners/${r}`)}>Remove</button></li>
		{/each}
	</ul>
	<form class="row" onsubmit={(e) => { e.preventDefault(); act('PUT', `/runners/${runner}`); runner = ''; }}>
		<input bind:value={runner} placeholder="runner account" required />
		<button>Add</button>
	</form>

	<h2>Danger</h2>
	<button onclick={() => { if (confirm(`Delete ${base} and everything in it?`)) act('DELETE', '').then(() => (location.href = '/')); }}>Delete project</button>
{/if}

<style>
	.x { border: none; background: none; padding: 0 2px; }
</style>
