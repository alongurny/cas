<script lang="ts">
	import { page } from '$app/state';
	import { get, api, when, short, user } from '$lib/api';
	import Diff from '$lib/Diff.svelte';
	let base = $derived(`${page.params.owner}/${page.params.project}`);
	let n = $derived(page.params.n);
	let p = $state<any>(null);
	let me = $state<any>(null);
	let err = $state('');
	let pick = $state<any>(null);
	let text = $state('');
	let suggestion = $state('');
	let replies = $state<Record<string, string>>({});
	async function load() {
		p = await get(`projects/${base}/proposals/${n}`).catch((e) => ((err = e.message), null));
	}
	$effect(() => {
		load();
		user().then((u) => (me = u));
	});
	async function act(method: string, path: string, body?: unknown) {
		err = '';
		try {
			await api(method, `projects/${base}/proposals/${n}${path}`, body);
			await load();
		} catch (x: any) {
			err = x.message;
		}
	}
	async function addThread(e: Event) {
		e.preventDefault();
		await act('POST', '/threads', { ...pick, text, suggestion: suggestion || undefined });
		pick = null;
		text = suggestion = '';
	}
</script>

{#if p}
	<div class="row">
		<span class="tag {p.state}">{p.state}</span>
		<h2 class="grow" style="margin: 0">#{p.number} {p.title}</h2>
	</div>
	<p class="muted">{p.author} proposes <span class="mono">{p.source}</span> → <span class="mono">{p.target}</span> · revision {p.revisions.length} · {when(p.updated)}</p>
	{#if err}<p class="error">{err}</p>{/if}

	<div class="panel">
		<strong>Gates</strong>
		<span class="tag" class:pass={p.gates.ok} class:fail={!p.gates.ok}>{p.gates.ok ? 'hold' : 'not yet'}</span>
		<span class="muted">approvals {p.gates.approvals}/{p.gates.approvals_needed}</span>
		{#each p.gates.checks as c (c.name)}<span class="tag {c.status}">{c.name}: {c.status}</span>{/each}
		{#if p.gates.resolved_needed}<span class="muted">· {p.gates.unresolved} unresolved</span>{/if}
		{#if p.state === 'open'}
			<div class="row" style="margin-top: 8px">
				{#if me && me.name !== p.author}<button onclick={() => act('POST', '/approve')}>Approve</button>{/if}
				<button class="primary" disabled={!p.gates.ok} onclick={() => act('POST', '/merge')}>Merge (rebase)</button>
				<button onclick={() => act('PATCH', '', { state: 'closed' })}>Close</button>
			</div>
		{:else if p.state === 'closed'}
			<button onclick={() => act('PATCH', '', { state: 'open' })}>Reopen</button>
		{:else if p.merged}
			<p>Merged as <a class="mono" href="/{base}/step?history={encodeURIComponent(p.target)}&node={encodeURIComponent(p.merged)}">{short(p.merged)}</a></p>
		{/if}
	</div>

	{#if p.diff}
		<h2>Changes <span class="muted">(tap a line to start a thread)</span></h2>
		<Diff lines={p.diff.lines.map((t: string) => ({ text: t }))} threads={p.threads} onpick={p.state === 'open' && me ? (x) => (pick = x) : undefined} />
	{/if}
	{#if pick}
		<form class="panel" onsubmit={addThread}>
			<p class="mono">{pick.path}:{pick.line} ({pick.side}) <span class="muted">{pick.text}</span></p>
			<textarea bind:value={text} placeholder="your note" required></textarea>
			<input bind:value={suggestion} placeholder="suggested replacement for the line (optional)" style="width: 100%" />
			<div class="row" style="margin-top: 6px"><button class="primary">Add thread</button><button type="button" onclick={() => (pick = null)}>Cancel</button></div>
		</form>
	{/if}

	<h2>Threads</h2>
	{#each p.threads as t (t.id)}
		<div class="panel" id="thread-{t.id}">
			<div class="row">
				<span class="mono">{t.path}:{t.line_now}</span>
				{#if t.outdated}<span class="tag closed">outdated</span>{/if}
				{#if t.resolved === 'true'}<span class="tag merged">resolved</span>{/if}
				{#if t.promote === 'true'}<span class="tag pass">becomes a step note</span>{/if}
				<span class="grow muted mono">{t.text}</span>
			</div>
			{#each t.comments as c, i (i)}
				<div><strong>{c.author}</strong> <span class="muted">{when(c.time)}</span><pre>{c.text}</pre>{#if c.suggestion}<pre class="mono sugg">{c.suggestion}</pre>{/if}</div>
			{/each}
			{#if p.state === 'open' && me}
				<div class="row">
					<input class="grow" bind:value={replies[t.id]} placeholder="reply" />
					<button onclick={() => act('POST', `/threads/${t.id}/comments`, { text: replies[t.id] }).then(() => (replies[t.id] = ''))}>Reply</button>
					<button onclick={() => act('POST', `/threads/${t.id}/resolve`, { resolved: t.resolved !== 'true' })}>{t.resolved === 'true' ? 'Unresolve' : 'Resolve'}</button>
					{#if me.name === p.author && t.side === 'new'}
						<button onclick={() => act('POST', `/threads/${t.id}/promote`, { promote: t.promote !== 'true' })}>{t.promote === 'true' ? 'Keep out of the notes' : 'Make it the step note'}</button>
					{/if}
				</div>
			{/if}
		</div>
	{:else}
		<p class="muted">No threads.</p>
	{/each}
{:else if err}
	<p class="error">{err}</p>
{/if}

<style>
	.sugg { background: var(--add); color: var(--add-text); padding: 2px 6px; }
</style>
