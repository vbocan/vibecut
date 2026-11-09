<script lang="ts">
	import { onMount } from 'svelte';
	import { appState as s } from '$lib/state.svelte';
	import { musicTone } from '$lib/stylecode';

	// "Use a style code": paste someone's code (or pick a saved style), see
	// what it sets, and "Make my video" makes it on this footage.
	onMount(() => void s.loadSavedStyles());

	let timer: ReturnType<typeof setTimeout> | null = null;
	function onInput(e: Event & { currentTarget: HTMLInputElement }) {
		const text = e.currentTarget.value;
		s.styleInput = text;
		if (timer) clearTimeout(timer);
		// Typing waits for a pause; a paste is read at once (onpaste).
		timer = setTimeout(() => void s.checkStyle(text), 400);
	}
	function onPaste(e: ClipboardEvent) {
		const text = e.clipboardData?.getData('text') ?? '';
		if (!text) return;
		e.preventDefault();
		if (timer) clearTimeout(timer);
		void s.checkStyle(text.trim());
	}
	function clear() {
		if (timer) clearTimeout(timer);
		s.clearStyle();
		document.getElementById('style-code-in')?.focus();
	}

	const p = $derived(s.stylePreview);
	const tone = $derived(p ? musicTone(p.music_match.how) : 'plain');
	// The Vibe chip is the heading; the rest are the details.
	const details = $derived(p ? p.style.chips.filter((c) => c.label !== 'Vibe') : []);
</script>

<section class="style-code" aria-labelledby="style-code-h">
	<div class="head">
		<h2 id="style-code-h">Use a style code</h2>
		<p class="sub">Got one from a friend? Their vibe, length, song and cuts — on your footage.</p>
	</div>

	<form
		class="field"
		class:active={!!p}
		onsubmit={(e) => {
			e.preventDefault();
			if (timer) clearTimeout(timer);
			void s.checkStyle(s.styleInput);
		}}
	>
		<span class="glyph" aria-hidden="true">❖</span>
		<label class="sr-only" for="style-code-in">Style code</label>
		<input
			id="style-code-in"
			placeholder="Paste a style code — VC1-…"
			value={s.styleInput}
			oninput={onInput}
			onpaste={onPaste}
			autocomplete="off"
			autocapitalize="off"
			spellcheck="false"
			aria-describedby="style-code-status"
		/>
		{#if s.styleInput}
			<button class="btn ghost small" type="button" onclick={clear}>Clear</button>
		{/if}
	</form>

	{#if s.savedStyles.length > 0}
		<div class="saved" role="group" aria-label="Your saved styles">
			<span class="eyebrow">Your styles</span>
			{#each s.savedStyles as st (st.file)}
				<button
					type="button"
					class="saved-style"
					class:on={p?.code === st.code}
					aria-pressed={p?.code === st.code}
					title={st.code}
					onclick={() => void s.checkStyle(st.code)}
				>
					{st.name} <span>{st.style.vibe_name}</span>
				</button>
			{/each}
		</div>
	{/if}

	<div id="style-code-status" aria-live="polite">
		{#if s.styleChecking}
			<p class="dim">Reading the code…</p>
		{:else if s.styleError}
			<p class="error">{s.styleError}</p>
		{:else if p}
			<div class="preview">
				<p class="using">
					<span class="badge">In use</span>
					<strong>{p.style.vibe_name}</strong>
					<span class="dim">— sets the vibe, length and song. Your clips, your title.</span>
				</p>
				<ul class="chips" aria-label="What the style sets">
					{#each details as c (c.label)}
						<li><span class="k">{c.label}</span> {c.value}</li>
					{/each}
				</ul>
				<p class="music {tone}"><span aria-hidden="true">♪</span> {p.music_match.note}</p>
			</div>
		{/if}
	</div>
</section>

<style>
	.style-code {
		display: flex;
		flex-direction: column;
		gap: 12px;
	}

	h2 {
		margin: 0;
		font-size: var(--fs-xl);
		font-weight: 700;
		letter-spacing: -0.02em;
	}

	.sub {
		margin: 2px 0 0;
		color: var(--text-dim);
		font-size: var(--fs-sm);
	}

	.field {
		display: flex;
		align-items: center;
		gap: 10px;
		padding: 6px 6px 6px 16px;
		border-radius: 999px;
		border: 1px solid var(--line-strong);
		background: rgba(255, 255, 255, 0.04);
		transition:
			border-color var(--dur-1) var(--ease-out),
			box-shadow var(--dur-1) var(--ease-out);
	}

	.field:focus-within {
		border-color: rgba(56, 214, 255, 0.55);
		box-shadow: 0 0 0 4px rgba(56, 214, 255, 0.12);
	}

	.field.active {
		border-color: rgba(255, 122, 69, 0.55);
		box-shadow: var(--glow);
	}

	.glyph {
		color: var(--accent);
		font-size: 15px;
	}

	input {
		flex: 1;
		min-width: 0;
		background: none;
		border: none;
		outline: none;
		padding: 8px 0;
		font-family: var(--mono);
		font-size: var(--fs-sm);
		text-overflow: ellipsis;
	}

	input::placeholder {
		font-family: var(--ui);
		color: var(--text-faint);
	}

	.small {
		padding: 6px 12px;
		font-size: var(--fs-xs);
	}

	.saved {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 6px;
	}

	.saved .eyebrow {
		margin-right: 4px;
	}

	.saved-style {
		border: 1px dashed var(--line-strong);
		background: transparent;
		color: var(--text);
		border-radius: 999px;
		padding: 4px 12px;
		font-size: var(--fs-xs);
		cursor: pointer;
	}

	.saved-style span {
		color: var(--text-faint);
		margin-left: 4px;
	}

	.saved-style:hover,
	.saved-style.on {
		border-style: solid;
		background: rgba(255, 255, 255, 0.06);
	}

	.saved-style.on {
		border-color: rgba(255, 122, 69, 0.6);
	}

	.preview {
		display: flex;
		flex-direction: column;
		gap: 10px;
		padding: 14px 16px;
		border-radius: var(--r-md);
		background: var(--grad-brand-soft);
		border: 1px solid var(--line);
		animation: rise var(--dur-2) var(--ease-out) both;
	}

	.using {
		margin: 0;
		display: flex;
		flex-wrap: wrap;
		align-items: baseline;
		gap: 6px;
	}

	.badge {
		font-size: 11px;
		font-weight: 700;
		letter-spacing: 0.1em;
		text-transform: uppercase;
		padding: 2px 8px;
		border-radius: 999px;
		background: var(--grad-cta);
		color: var(--on-cta);
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
		background: rgba(7, 8, 12, 0.35);
		font-size: var(--fs-xs);
	}

	.chips .k {
		color: var(--text-faint);
		margin-right: 2px;
	}

	.music {
		margin: 0;
		font-size: var(--fs-sm);
		color: var(--text-dim);
	}

	.music.good span {
		color: var(--good);
	}

	.music.warn span {
		color: var(--warn);
	}

	.dim {
		color: var(--text-faint);
		margin: 0;
	}

	.using .dim {
		font-size: var(--fs-sm);
	}

	.error {
		color: var(--bad);
		margin: 0;
	}

	@keyframes rise {
		from {
			opacity: 0;
			transform: translateY(8px);
		}
	}
</style>
