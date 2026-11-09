<script lang="ts">
	import { appState as s } from '$lib/state.svelte';
	import type { StyleShare } from '$lib/api';
	import { ApiError } from '$lib/api';

	// "Share this style": the project's creative choices as a short code
	// anyone can paste into Create. Made when the panel opens, and again
	// whenever the project changes while it's open.
	let open = $state(false);
	let share = $state<StyleShare | null>(null);
	let madeFor = $state('');
	let busy = $state(false);
	let error = $state<string | null>(null);
	let copied = $state(false);
	/** Share "let Vibecut pick" instead of this song. */
	let pickInstead = $state(false);
	let name = $state('');
	let saving = $state(false);
	let saved = $state<string | null>(null);
	let codeEl: HTMLInputElement | undefined = $state();

	const hasSong = $derived(!!s.proposeResult?.soundtrack || !!s.configStructured?.soundtrack);
	// What the code depends on: the config, the version shown (its song
	// credit) and the music choice.
	const key = $derived(`${s.proposeResult?.config_path ?? ''}\n${s.configToml}\n${s.manifestPath ?? ''}\n${pickInstead}`);

	$effect(() => {
		if (open && s.proposeResult && key !== madeFor && !busy) void make(key);
	});

	async function make(k: string) {
		busy = true;
		error = null;
		copied = false;
		saved = null;
		try {
			const r = await s.shareStyle({ letVibecutPick: pickInstead });
			share = r;
			madeFor = k;
		} catch (e) {
			error = e instanceof ApiError ? e.describe() : e instanceof Error ? e.message : String(e);
			madeFor = k;
		} finally {
			busy = false;
		}
	}

	async function copy() {
		if (!share) return;
		error = null;
		saved = null;
		try {
			await navigator.clipboard.writeText(share.code);
			copied = true;
		} catch {
			// No async clipboard (plain http on a LAN, a locked-down
			// browser): copy the selected field the old way, else leave it
			// selected for Ctrl C.
			codeEl?.focus();
			codeEl?.select();
			copied = document.execCommand('copy');
			if (!copied) error = 'Your browser blocked copying — the code is selected, press Ctrl C.';
		}
	}

	async function save(e: SubmitEvent) {
		e.preventDefault();
		const n = name.trim();
		if (!share || !n || saving) return;
		saving = true;
		error = null;
		try {
			const r = await s.saveStyle(n, share.code);
			// The newest outcome is the one the status line reads.
			copied = false;
			saved = `Saved as “${r.name}” — it’s under Use a style code in Create.`;
			name = '';
		} catch (err) {
			error = err instanceof ApiError ? err.describe() : String(err);
		} finally {
			saving = false;
		}
	}

	const changes = $derived(share?.style.settings.length ?? 0);
</script>

<div class="style-share">
	<button class="disclosure" type="button" aria-expanded={open} aria-controls="style-share-body" onclick={() => (open = !open)}>
		<span class="eyebrow">Style</span>
		<span class="now">Share this style</span>
		<span class="chev" aria-hidden="true">{open ? '−' : '+'}</span>
	</button>

	{#if open}
		<div class="body" id="style-share-body">
			<p class="explain">
				A short code for this video's look — vibe, length, song, cuts, title card and formats. Not your clips, not
				your title. Paste it under <strong>Use a style code</strong> in Create, on any footage.
			</p>

			{#if share}
				<div class="code-row" class:stale={busy}>
					<label class="sr-only" for="style-code-out">Style code</label>
					<input
						id="style-code-out"
						class="code"
						readonly
						value={share.code}
						bind:this={codeEl}
						onfocus={(e) => e.currentTarget.select()}
					/>
					<button class="btn primary copy" type="button" disabled={busy} onclick={copy}>
						{copied ? 'Copied' : 'Copy'}
					</button>
				</div>
				<p class="meta">
					{share.chars} characters ·
					{changes === 0 ? `${share.style.vibe_name} as it comes` : `${share.style.vibe_name} with ${changes} change${changes === 1 ? '' : 's'}`}
				</p>
				<ul class="chips" aria-label="What the code sets">
					{#each share.style.chips as c (c.label)}
						<li><span class="k">{c.label}</span> {c.value}</li>
					{/each}
				</ul>
				{#if hasSong}
					<label class="toggle">
						<input type="checkbox" bind:checked={pickInstead} disabled={busy} />
						Let Vibecut pick their song instead of this one
					</label>
				{/if}
				<form class="save" onsubmit={save}>
					<label class="sr-only" for="style-name">Name this style to keep it</label>
					<input id="style-name" placeholder="Name it to keep it (optional)" maxlength="60" bind:value={name} autocomplete="off" />
					<button class="btn small" type="submit" disabled={!name.trim() || saving}>{saving ? 'Saving…' : 'Save'}</button>
				</form>
			{:else if busy}
				<p class="dim">Making the code…</p>
			{/if}

			<div aria-live="polite">
				{#if error}
					<p class="error">{error}</p>
				{:else if copied}
					<p class="ok">Copied. Send it to anyone with Vibecut.</p>
				{:else if saved}
					<p class="ok">{saved}</p>
				{/if}
			</div>
		</div>
	{/if}
</div>

<style>
	.disclosure {
		width: 100%;
		display: grid;
		grid-template-columns: auto 1fr auto;
		align-items: center;
		gap: 10px;
		background: none;
		border: none;
		padding: 0;
		cursor: pointer;
		text-align: left;
	}

	.now {
		font-family: var(--serif);
		font-size: 17px;
	}

	.chev {
		font-size: 20px;
		color: var(--text-dim);
	}

	.body {
		display: flex;
		flex-direction: column;
		gap: 10px;
		margin-top: 14px;
	}

	.explain {
		margin: 0;
		font-size: var(--fs-sm);
		color: var(--text-dim);
	}

	.explain strong {
		color: var(--text);
		font-weight: 600;
	}

	.code-row {
		display: flex;
		align-items: center;
		gap: 6px;
		padding: 5px 5px 5px 14px;
		border-radius: 999px;
		border: 1px solid var(--line-strong);
		background: rgba(255, 255, 255, 0.04);
		transition: opacity var(--dur-2) var(--ease-out);
	}

	.code-row.stale {
		opacity: 0.55;
	}

	.code-row:focus-within {
		border-color: rgba(56, 214, 255, 0.55);
	}

	.code {
		flex: 1;
		min-width: 0;
		background: none;
		border: none;
		outline: none;
		padding: 6px 0;
		font-family: var(--mono);
		font-size: var(--fs-xs);
		color: var(--text);
		text-overflow: ellipsis;
	}

	.copy {
		padding: 7px 16px;
	}

	.meta {
		margin: -2px 6px 0;
		font-size: var(--fs-xs);
		color: var(--text-faint);
	}

	.chips {
		list-style: none;
		margin: 0;
		padding: 0;
		display: flex;
		flex-wrap: wrap;
		gap: 6px;
	}

	.chips li {
		padding: 3px 10px;
		border-radius: 999px;
		border: 1px solid var(--line);
		background: rgba(255, 255, 255, 0.03);
		font-size: var(--fs-xs);
		color: var(--text);
	}

	.chips .k {
		color: var(--text-faint);
		margin-right: 2px;
	}

	.toggle {
		display: flex;
		align-items: center;
		gap: 8px;
		font-size: var(--fs-sm);
		color: var(--text-dim);
		cursor: pointer;
	}

	.save {
		display: flex;
		gap: 6px;
	}

	.save input {
		flex: 1;
		min-width: 0;
		background: rgba(255, 255, 255, 0.04);
		border: 1px solid var(--line);
		border-radius: 999px;
		padding: 7px 14px;
		font-size: var(--fs-sm);
	}

	.small {
		padding: 6px 14px;
		font-size: var(--fs-xs);
	}

	.dim {
		margin: 0;
		color: var(--text-faint);
	}

	.ok {
		margin: 0;
		color: var(--good);
		font-size: var(--fs-sm);
	}

	.error {
		margin: 0;
		color: var(--bad);
		font-size: var(--fs-sm);
	}
</style>
