<script lang="ts">
	import { api, setToken, forget } from '#lib/api.ts';
	let name = $state('');
	let password = $state('');
	let err = $state('');
	async function submit(e: Event) {
		e.preventDefault();
		err = '';
		try {
			const r = await api('POST', 'login', { name, password });
			setToken(r.token);
			forget();
			location.href = '/';
		} catch (x: any) {
			err = x.message;
		}
	}
</script>

<h1>Sign in</h1>
<form class="panel" onsubmit={submit} style="max-width: 360px">
	<p><input bind:value={name} placeholder="name" autocomplete="username" required style="width: 100%" /></p>
	<p><input type="password" bind:value={password} placeholder="password" autocomplete="current-password" required style="width: 100%" /></p>
	<button class="primary">Sign in</button>
	{#if err}<p class="error">{err}</p>{/if}
</form>
