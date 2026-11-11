<script lang="ts">
	import { page } from '$app/state';
	import { api } from '#lib/api.ts';
	import { goto } from '$app/navigation';
	let base = $derived(`${page.params.owner}/${page.params.project}`);
	let title = $state('');
	let body = $state('');
	let err = $state('');
	async function submit(e: Event) {
		e.preventDefault();
		try {
			const r = await api('POST', `projects/${base}/issues`, { title, body });
			goto(`/${base}/issues/${r.number}`);
		} catch (x: any) {
			err = x.message;
		}
	}
</script>

<form class="panel" onsubmit={submit}>
	<p><input bind:value={title} placeholder="title" required style="width: 100%" /></p>
	<textarea bind:value={body} placeholder="what happened"></textarea>
	<button class="primary">Open issue</button>
	{#if err}<span class="error">{err}</span>{/if}
</form>
