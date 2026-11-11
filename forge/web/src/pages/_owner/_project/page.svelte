<script lang="ts">
	import { page } from '$app/state';
	import { get, short, ApiError } from '#lib/api.ts';
	let p = $state<any>(null);
	let err = $state('');
	let base = $derived(`${page.params.owner}/${page.params.project}`);
	$effect(() => {
		get(`projects/${base}`).then((x) => (p = x)).catch((e: ApiError) => (err = e.message));
	});
</script>

{#if err}<p class="error">{err}</p>{/if}
{#if p}
	<p class="muted">{p.description ?? ''} <span class="tag">{p.visibility}</span> <span class="tag">your role: {p.role}</span></p>
	{#if p.fork}<p class="muted">Fork of <a href="/{p.fork}">{p.fork}</a></p>{/if}
	{#if p.releases?.length}
		<div class="panel">
			<strong>Latest release</strong>
			{#each p.releases[0].tags as t (t)}<span class="tag">{t}</span>{/each}
			<pre>{p.releases[0].changelog ?? ''}</pre>
		</div>
	{/if}
	<h2>Histories</h2>
	<ul class="list panel">
		{#each p.histories as h (h.name)}
			<li class="row">
				<a href="/{base}/log?history={encodeURIComponent(h.name)}" class="grow mono">{h.name}</a>
				<span class="muted mono">{h.tips.map(short).join(', ')}</span>
				<a href="/{base}/tree?history={encodeURIComponent(h.name)}">files</a>
			</li>
		{:else}
			<li class="muted">
				Empty. Push a private history and propose it:
				<pre>cas --store https://{location.host}/p/{base} --token-file TOKEN commit ...</pre>
			</li>
		{/each}
	</ul>
	{#if p.readme}
		<h2>{p.readme.path}</h2>
		<div class="panel"><pre>{p.readme.text}</pre></div>
	{/if}
{/if}
