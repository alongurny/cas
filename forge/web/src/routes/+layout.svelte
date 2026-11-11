<script lang="ts">
	import '../app.css';
	import { user, setToken, forget, api } from '#lib/api.ts';
	let { children } = $props();
	let me = $state<any>(null);
	$effect(() => {
		user().then((u) => (me = u));
	});
	async function signOut() {
		await api('POST', 'logout').catch(() => {});
		setToken(null);
		forget();
		location.href = '/';
	}
</script>

<header>
	<div class="bar">
		<a class="brand" href="/">cas forge</a>
		<span class="grow"></span>
		{#if me}
			<a href="/user/{me.name}">{me.name}</a>
			<button onclick={signOut}>Sign out</button>
		{:else}
			<a href="/login">Sign in</a>
		{/if}
	</div>
</header>
<main>{@render children()}</main>

<style>
	header { border-bottom: 1px solid var(--line); background: var(--panel); }
	.bar { max-width: 1100px; margin: 0 auto; padding: 8px 16px; display: flex; gap: 12px; align-items: center; }
	.brand { font-weight: 700; color: var(--text); }
</style>
