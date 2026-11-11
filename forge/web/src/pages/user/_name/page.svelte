<script lang="ts">
	import { page } from '$app/state';
	import { get, api, user, when } from '#lib/api.ts';
	let account = $state<any>(null);
	let me = $state<any>(null);
	let tokens = $state<any[]>([]);
	let label = $state('');
	let made = $state('');
	let err = $state('');
	$effect(() => {
		const n = page.params.name;
		get(`users/${n}`).then((a) => (account = a));
		user().then((u) => {
			me = u;
			if (u && u.name === n) get('user/tokens').then((t) => (tokens = t));
		});
	});
	async function makeToken(e: Event) {
		e.preventDefault();
		try {
			made = (await api('POST', 'user/tokens', { label: label || 'token' })).token;
			tokens = await get('user/tokens');
		} catch (x: any) {
			err = x.message;
		}
	}
	async function revoke(key: string) {
		await api('DELETE', `user/tokens/${encodeURIComponent(key)}`);
		tokens = await get('user/tokens');
	}
</script>

{#if account}
	<h1>{account.display ?? account.name} <span class="muted">{account.kind}</span></h1>
	{#if me && me.name === account.name}
		<h2>Tokens</h2>
		<ul class="list panel">
			{#each tokens as t (t.key)}
				<li class="row">
					<span class="grow">{t.label} <span class="muted">{t.scope} · made {when(t.created)}{t.expires ? ' · expires ' + when(t.expires) : ''}</span></span>
					<button onclick={() => revoke(t.key)}>Revoke</button>
				</li>
			{/each}
		</ul>
		<form class="row" onsubmit={makeToken}>
			<input bind:value={label} placeholder="what it is for" />
			<button>Make a token</button>
		</form>
		{#if made}<p class="panel">Your new token, shown once: <code>{made}</code></p>{/if}
		{#if err}<p class="error">{err}</p>{/if}
	{/if}
{/if}
