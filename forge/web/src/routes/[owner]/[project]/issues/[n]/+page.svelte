<script lang="ts">
	import { page } from '$app/state';
	import { get, api, when } from '$lib/api';
	let base = $derived(`${page.params.owner}/${page.params.project}`);
	let n = $derived(page.params.n);
	let i = $state<any>(null);
	let project = $state<any>(null);
	let text = $state('');
	let err = $state('');
	async function load() {
		i = await get(`projects/${base}/issues/${n}`).catch((e) => ((err = e.message), null));
	}
	$effect(() => {
		load();
		get(`projects/${base}`).then((p) => (project = p));
	});
	async function act(method: string, path: string, body: unknown) {
		err = '';
		try {
			await api(method, `projects/${base}/issues/${n}${path}`, body);
			await load();
		} catch (x: any) {
			err = x.message;
		}
	}
	let labels = $derived(project ? Object.keys(project.labels ?? {}) : []);
</script>

{#if i}
	<div class="row"><span class="tag {i.state}">{i.state}</span><h2 class="grow" style="margin: 0">#{i.number} {i.title}</h2></div>
	<p class="muted">{i.author} opened {when(i.created)}</p>
	{#if err}<p class="error">{err}</p>{/if}
	<div class="row">
		{#each i.labels as l (l)}<span class="tag">{l} <button class="x" onclick={() => act('PATCH', '', { remove_labels: [l] })}>×</button></span>{/each}
		{#if labels.length}
			<select onchange={(e) => { const v = (e.target as HTMLSelectElement).value; if (v) act('PATCH', '', { add_labels: [v] }); (e.target as HTMLSelectElement).value = ''; }}>
				<option value="">add label</option>
				{#each labels as l (l)}<option>{l}</option>{/each}
			</select>
		{/if}
		<span class="grow"></span>
		<button onclick={() => act('PATCH', '', { state: i.state === 'open' ? 'closed' : 'open' })}>{i.state === 'open' ? 'Close' : 'Reopen'}</button>
	</div>
	{#if i.anchor}<p class="mono">{i.anchor.path}:{i.anchor.line} <span class="muted">{i.anchor.quote ?? ''}</span></p>{/if}
	{#if i.body}<div class="panel"><pre>{i.body}</pre></div>{/if}
	{#if i.suggestion}<div class="panel"><strong>Suggestion</strong><pre class="mono">{i.suggestion}</pre></div>{/if}
	{#each i.comments as c, k (k)}
		<div class="panel"><strong>{c.author}</strong> <span class="muted">{when(c.time)}</span><pre>{c.text}</pre></div>
	{/each}
	<form class="panel" onsubmit={(e) => { e.preventDefault(); act('POST', '/comments', { text }).then(() => (text = '')); }}>
		<textarea bind:value={text} placeholder="comment" required></textarea>
		<button class="primary">Comment</button>
	</form>
{:else if err}
	<p class="error">{err}</p>
{/if}

<style>
	.x { border: none; background: none; padding: 0 2px; }
</style>
