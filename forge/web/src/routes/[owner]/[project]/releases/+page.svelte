<script lang="ts">
	import { page } from '$app/state';
	import { get, api, short } from '#lib/api.ts';
	let base = $derived(`${page.params.owner}/${page.params.project}`);
	let list = $state<any[]>([]);
	let tag = $state('');
	let changelog = $state('');
	let compatible = $state(false);
	let err = $state('');
	async function load() {
		list = await get(`projects/${base}/releases`);
	}
	$effect(() => {
		load();
	});
	async function publish(e: Event) {
		e.preventDefault();
		err = '';
		try {
			await api('POST', `projects/${base}/releases`, { tag, changelog, compatible });
			tag = changelog = '';
			await load();
		} catch (x: any) {
			err = x.message;
		}
	}
</script>

{#each list as r (r.id)}
	<div class="panel">
		<div class="row">
			{#each r.tags as t (t)}<strong>{t}</strong>{/each}
			<span class="tag">{r.compatible ? 'compatible' : 'breaking'}</span>
			<a class="mono" href="/{base}/tree?history=h%2Frelease&node={encodeURIComponent(r.id)}">{short(r.id)}</a>
			<span class="grow"></span>
			{#each r.tags as t (t)}<a href="/api/v1/projects/{base}/releases/{encodeURIComponent(t)}/archive.tar.gz">{t}.tar.gz</a>{/each}
		</div>
		<pre>{r.changelog ?? ''}</pre>
	</div>
{:else}
	<p class="muted">No releases yet.</p>
{/each}

<h2>Publish the tip of h/dev</h2>
<form class="panel" onsubmit={publish}>
	<p class="row"><input bind:value={tag} placeholder="tag, e.g. v1.2" required /> <label><input type="checkbox" bind:checked={compatible} /> compatible with the previous release</label></p>
	<textarea bind:value={changelog} placeholder="changelog: what is different from the previous release" required></textarea>
	<button class="primary">Publish</button>
	{#if err}<span class="error">{err}</span>{/if}
</form>
