// The forge's JSON API (/api/v1). The session token lives in a cookie the
// forge also reads, so raw downloads work as links.

export type J = any; // eslint-disable-line @typescript-eslint/no-explicit-any

export function token(): string | null {
	const m = document.cookie.match(/(?:^|;\s*)cas_token=([^;]+)/);
	return m ? decodeURIComponent(m[1]) : null;
}

export function setToken(t: string | null, days = 30) {
	const secure = location.protocol === 'https:' ? '; Secure' : '';
	if (t) document.cookie = `cas_token=${encodeURIComponent(t)}; Path=/; Max-Age=${days * 86400}; SameSite=Strict${secure}`;
	else document.cookie = `cas_token=; Path=/; Max-Age=0; SameSite=Strict${secure}`;
}

export class ApiError extends Error {
	constructor(public status: number, message: string) {
		super(message);
	}
}

export async function api(method: string, path: string, body?: unknown): Promise<J> {
	const headers: Record<string, string> = {};
	const t = token();
	if (t) headers['Authorization'] = `Bearer ${t}`;
	if (body !== undefined) headers['Content-Type'] = 'application/json';
	const r = await fetch(`/api/v1/${path}`, { method, headers, body: body === undefined ? undefined : JSON.stringify(body) });
	const j = await r.json().catch(() => null);
	if (!r.ok) throw new ApiError(r.status, j?.message ?? r.statusText);
	return j;
}

export const get = (path: string) => api('GET', path);

/** Query string from an object, leaving out empty values. */
export function qs(o: Record<string, string | number | undefined | null>): string {
	const p = new URLSearchParams();
	for (const [k, v] of Object.entries(o)) if (v !== undefined && v !== null && v !== '') p.set(k, String(v));
	const s = p.toString();
	return s ? `?${s}` : '';
}

/** A short form of a node id or view: the start of its key. */
export function short(id: string | null | undefined): string {
	if (!id) return '';
	const key = id.split(' ').pop() ?? id;
	const hex = key.includes(':') ? key.split(':')[1] : key;
	const occ = id.includes('#') ? '#' + id.split('#').pop() : '';
	return hex.slice(0, 10) + occ;
}

export function when(t: string | number | undefined): string {
	const n = Number(t);
	if (!n) return '';
	return new Date(n * 1000).toLocaleString();
}

/** The signed-in user, cached for the page's life. */
let me: Promise<J | null> | null = null;
export function user(): Promise<J | null> {
	if (!token()) return Promise.resolve(null);
	me ??= get('user').catch(() => null);
	return me;
}
export function forget() {
	me = null;
}
