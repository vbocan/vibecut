<script lang="ts">
	import { appState as s } from '$lib/state.svelte';
	import { tellCreate, tellStudio } from '$lib/tellit';

	// Plain-language requests. Simple ones ("shorter", "new song", "drop the
	// bit at 0:42") are understood locally and work without an API key;
	// anything else goes to the AI when the server has one.
	const studio = $derived(s.proposeResult !== null && (s.renderHistory.length > 0 || s.renderMode === 'done'));
	let note = $state<string | null>(null);

	const examples = $derived(
		studio
			? ['shorter', 'drop the bit at 0:05', 'new song', 'more energy']
			: ['a 30 second action reel', 'slow drone film', 'kids, under a minute']
	);

	function nextSong(): string | null {
		if (s.tracks.length === 0) return null;
		const i = s.tracks.findIndex((t) => t.path === s.effectiveTrack?.path);
		return s.tracks[(i + 1) % s.tracks.length].path;
	}

	async function runStudio(text: string): Promise<boolean> {
		const r = tellStudio(text);
		switch (r.kind) {
			case 'action':
				await s.tweakAndRender(r.action, r.label);
				break;
			case 'song': {
				const p = nextSong();
				if (!p) return false;
				await s.changeSong(p);
				break;
			}
			case 'mix':
				await s.anotherMix();
				break;
			case 'drop_at': {
				const g = s.segmentAt(r.seconds);
				if (!g) {
					note = `There's no moment at that time.`;
					return true;
				}
				await s.withUndo(r.label, () => s.dropSegment(g));
				break;
			}
			case 'source':
				await s.withUndo(r.label, () =>
					r.more ? s.moreOfSource(r.sourceId) : s.lessOfSource(r.sourceId)
				);
				break;
			default:
				return false;
		}
		note = `Understood: ${r.label}.`;
		return true;
	}

	function runCreate(text: string): boolean {
		const r = tellCreate(text);
		if (r.kind !== 'plan') return false;
		if (r.vibe) s.selectedVibe = r.vibe;
		if (r.duration) s.targetDuration = r.duration;
		const vibe = s.vibes.find((v) => v.key === s.effectiveVibe)?.name ?? 'your vibe';
		note = `Understood: ${vibe}${r.duration ? `, ${r.duration}` : ''}.`;
		void s.makeVideo();
		return true;
	}

	/** A request that came in mid-render, waiting on "Stop and apply this instead". */
	let offer = $state<string | null>(null);

	async function stopAndApply() {
		const text = offer;
		if (!text) return;
		await s.stopRender({ quiet: true });
		if (s.rendering) return; // couldn't stop; the toast says why
		offer = null;
		s.briefText = text;
		await submit();
	}

	async function submit() {
		const text = s.briefText.trim();
		if (!text || s.briefBusy) return;
		note = null;
		offer = null;
		s.briefError = null;
		if (studio && s.rendering) {
			note = 'Still rendering the last change.';
			offer = text;
			return;
		}
		const handled = studio ? await runStudio(text) : runCreate(text);
		if (handled) {
			s.briefText = '';
			return;
		}
		if (!s.aiAvailable) {
			note = `I didn't catch that. Try: ${examples.join(' · ')}.`;
			return;
		}
		const wasStudio = studio;
		await s.submitBrief();
		if (s.briefError) return;
		if (wasStudio) void s.startRenderAsync();
		else if (s.effectiveVibe && s.folder) void s.makeVideo();
	}
</script>

<form
	class="cmd"
	onsubmit={(e) => {
		e.preventDefault();
		void submit();
	}}
>
	<span class="spark" aria-hidden="true">✦</span>
	<label class="sr-only" for="cmd-input">{studio ? 'Describe a change' : 'Describe the video you want'}</label>
	<input
		id="cmd-input"
		placeholder={studio ? `Tell it — e.g. “${examples[1]}”` : `Or just say it — e.g. “${examples[0]}”`}
		bind:value={s.briefText}
		disabled={s.briefBusy}
		autocomplete="off"
	/>
	<kbd class="hint" aria-hidden="true">Ctrl K</kbd>
	<button class="btn primary" type="submit" disabled={s.briefBusy || !s.briefText.trim()}>
		{s.briefBusy ? 'Thinking…' : studio ? 'Apply' : 'Make it'}
	</button>
</form>
<div class="feedback" aria-live="polite">
	{#if s.briefError}
		<p class="error" role="alert">{s.briefError}</p>
	{:else if note}
		<p class="why">
			{offer && !s.rendering ? 'The last change is ready.' : note}
			{#if offer}
				<button class="btn small offer" type="button" disabled={s.stopping} onclick={stopAndApply}>
					{s.stopping ? 'Stopping…' : s.rendering ? 'Stop and apply this instead' : 'Apply it now'}
				</button>
			{/if}
		</p>
	{:else if s.briefRationale}
		<p class="why"><span class="serif">“{s.briefRationale}”</span></p>
	{/if}
</div>
<div class="examples" aria-label="Examples">
	{#each examples as ex (ex)}
		<button type="button" class="ex" onclick={() => (s.briefText = ex)}>{ex}</button>
	{/each}
	{#if !s.aiAvailable}<span class="local">Works offline — no AI key needed</span>{/if}
</div>

<style>
	.cmd {
		display: flex;
		align-items: center;
		gap: 10px;
		padding: 6px 6px 6px 16px;
		border-radius: 999px;
		border: 1px solid var(--line-strong);
		background: rgba(255, 255, 255, 0.04);
	}

	.cmd:focus-within {
		border-color: rgba(184, 243, 90, 0.55);
		box-shadow: 0 0 0 4px rgba(184, 243, 90, 0.12);
	}

	.spark {
		color: var(--lime);
	}

	input {
		flex: 1;
		min-width: 0;
		background: none;
		border: none;
		outline: none;
		padding: 8px 0;
	}

	.hint {
		font-family: var(--mono);
		font-size: 11px;
		color: var(--text-faint);
		border: 1px solid var(--line);
		border-radius: 6px;
		padding: 1px 6px;
	}

	.examples {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 6px;
		margin-top: 10px;
	}

	.ex {
		border: 1px dashed var(--line-strong);
		background: transparent;
		color: var(--text-dim);
		border-radius: 999px;
		padding: 4px 12px;
		font-size: var(--fs-xs);
		cursor: pointer;
	}

	.ex:hover {
		color: var(--text);
		border-style: solid;
	}

	.local {
		font-size: var(--fs-xs);
		color: var(--text-faint);
		margin-left: 4px;
	}

	.why {
		margin: 10px 4px 0;
		color: var(--text-dim);
	}

	.why .serif {
		font-size: 17px;
	}

	.offer {
		margin-left: 8px;
		padding: 5px 12px;
		font-size: var(--fs-xs);
	}

	.error {
		color: var(--bad);
		margin: 10px 4px 0;
	}

	@media (max-width: 560px) {
		.hint {
			display: none;
		}
	}
</style>
