<script lang="ts">
	import { page } from '$app/state';
	let { children } = $props();
	let base = $derived(`/${page.params.owner}/${page.params.project}`);
	const tabs = [
		['', 'Overview'],
		['/tree', 'Code'],
		['/log', 'Log'],
		['/proposals', 'Proposals'],
		['/issues', 'Issues'],
		['/releases', 'Releases'],
		['/settings', 'Settings']
	];
	function on(t: string) {
		const p = page.url.pathname;
		if (t === '') return p === base;
		if (t === '/tree') return p.startsWith(base + '/tree');
		if (t === '/log') return p.startsWith(base + '/log') || p.startsWith(base + '/step');
		return p.startsWith(base + t);
	}
</script>

<div class="row"><h1><a href="/{page.params.owner}">{page.params.owner}</a> / <a href={base}>{page.params.project}</a></h1></div>
<nav class="tabs">
	{#each tabs as [t, label] (t)}
		<a href="{base}{t}" class:on={on(t)}>{label}</a>
	{/each}
</nav>
{@render children()}
