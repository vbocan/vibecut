<script lang="ts">
	import { appState as s } from '$lib/state.svelte';
	import { formatDuration, parseDuration } from '$lib/finetune';
	import { radiogroup } from '$lib/radiogroup';
	import {
		ANCHOR_GRID,
		ANCHOR_LABELS,
		FONTS,
		colorInputValue,
		cssColor,
		fontCss,
		layerOpacity,
		newCard,
		newTextLine,
		placement,
		placementStyle
	} from '$lib/titles';

	// The opening and closing cards: their look, timing and every line of
	// text, with a preview laid out the way the renderer lays it out. Each
	// change is written into the project's TOML in place (Fine-tune's patch),
	// one at a time so quick edits can't overwrite each other.
	const uid = $props.id();
	let which = $state<'intro' | 'outro'>('intro');
	let selected = $state(0);
	/** Values being dragged or typed, by dotted key, until they are saved. */
	let live = $state<Record<string, unknown>>({});
	let t = $state(1.5);
	let previewW = $state(560);
	let note = $state('');
	let chain: Promise<unknown> = Promise.resolve();

	// eslint-disable-next-line @typescript-eslint/no-explicit-any
	type Any = any;
	const cfg = $derived(s.configStructured as Any);
	const card = $derived((cfg?.[which] ?? null) as Any);
	const layers = $derived((card?.layers ?? []) as Any[]);
	const title = $derived(s.proposeResult?.title || s.title || s.folderLabel || 'My video');
	const frame = $derived.by(() => {
		const r = /^(\d+)x(\d+)$/.exec(String(cfg?.outputs?.[0]?.resolution ?? ''));
		return r ? { w: Number(r[1]), h: Number(r[2]) } : { w: 1920, h: 1080 };
	});
	const scale = $derived(previewW / frame.w);
	const dur = $derived(Math.max(0.5, parseDuration(val('duration', card?.duration)) || 4));
	const sel = $derived(layers[Math.min(selected, Math.max(0, layers.length - 1))] ?? null);
	const selIndex = $derived(Math.min(selected, Math.max(0, layers.length - 1)));
	const cardLive = $derived(
		card
			? { duration: String(val('duration', card.duration)), fade_in: String(val('fade_in', card.fade_in)), fade_out: String(val('fade_out', card.fade_out)) }
			: null
	);

	$effect(() => {
		// Keep the preview time inside the card.
		if (t > dur) t = Math.min(dur, 1.5);
	});

	/** The value of `rest` (under the card) as shown: being edited, else saved. */
	function val(rest: string, saved: unknown): unknown {
		const key = `${which}.${rest}`;
		return key in live ? live[key] : saved;
	}

	function lv(i: number, field: string, saved: unknown): Any {
		return val(`layers.${i}.${field}`, saved);
	}

	function edit(rest: string, value: unknown) {
		live[`${which}.${rest}`] = value;
	}

	function save(changes: { key: string; value: unknown }[], said: string) {
		chain = chain.then(async () => {
			const ok = await s.tune(changes);
			for (const c of changes) delete live[c.key];
			note = ok ? said : (s.tuneError ?? '');
		});
		return chain;
	}

	function commit(rest: string, value: unknown, said = 'Saved.') {
		const key = `${which}.${rest}`;
		live[key] = value;
		return save([{ key, value }], said);
	}

	const cardName = $derived(which === 'intro' ? 'opening card' : 'closing card');
	const anCard = $derived(which === 'intro' ? 'an opening card' : 'a closing card');

	async function addCard() {
		const vibeCard = (s.tuneDefaults?.defaults as Any)?.[which] ?? null;
		await save([{ key: which, value: newCard(which, title, vibeCard) }], `Added ${anCard}.`);
		selected = 0;
	}

	async function removeCard() {
		await save([{ key: which, value: null }], `Removed the ${cardName}.`);
	}

	async function addLine() {
		const lastText = [...layers].reverse().find((l) => l.kind === 'text') ?? null;
		const i = layers.length;
		await save([{ key: `${which}.layers.${i}`, value: newTextLine(lastText) }], 'Added a line of text.');
		selected = i;
	}

	async function removeLine(i: number) {
		await save([{ key: `${which}.layers.${i}`, value: null }], 'Removed the line.');
		selected = Math.max(0, i - 1);
	}

	function secs(v: unknown, dflt: number): number {
		const n = parseDuration(v);
		return Number.isFinite(n) ? n : dflt;
	}

	/** Box colour and opacity ↔ `#RRGGBBAA`. */
	function boxColor(c: unknown): { rgb: string; alpha: number } {
		const hex = String(c ?? '').replace(/^#|^0x/i, '');
		const alpha = /^[0-9a-f]{8}$/i.test(hex) ? parseInt(hex.slice(6, 8), 16) / 255 : 1;
		return { rgb: colorInputValue(String(c ?? ''), '#000000'), alpha };
	}
	function boxValue(rgb: string, alpha: number): string {
		const a = Math.round(Math.max(0, Math.min(1, alpha)) * 255)
			.toString(16)
			.padStart(2, '0');
		return `${rgb}${a}`;
	}

	function short(text: unknown): string {
		const v = String(text ?? '').trim();
		return v.length > 22 ? `${v.slice(0, 21)}…` : v || '(empty)';
	}
</script>

<div class="titles">
	<div class="which" role="tablist" aria-label="Card" use:radiogroup>
		<button
			type="button"
			role="tab"
			aria-selected={which === 'intro'}
			class:on={which === 'intro'}
			onclick={() => {
				which = 'intro';
				selected = 0;
			}}>Opening{cfg?.intro ? '' : ' (none)'}</button
		>
		<button
			type="button"
			role="tab"
			aria-selected={which === 'outro'}
			class:on={which === 'outro'}
			onclick={() => {
				which = 'outro';
				selected = 0;
			}}>Closing{cfg?.outro ? '' : ' (none)'}</button
		>
	</div>

	{#if !card}
		<div class="empty">
			<p>
				{which === 'intro'
					? 'No opening card: the video starts straight on the first shot.'
					: 'No closing card: the video ends on the last shot.'}
			</p>
			<button class="btn primary" type="button" onclick={addCard}>Add {anCard}</button>
		</div>
	{:else}
		<!-- ---------- preview ---------- -->
		{@const mode = String(val('mode', card.mode))}
		<div
			class="preview"
			bind:clientWidth={previewW}
			style="aspect-ratio: {frame.w} / {frame.h}; {mode === 'card' ? `background: ${cssColor(String(val('bg_color', card.bg_color)), '#0a1628')}` : ''}"
			class:overlay={mode !== 'card'}
			role="img"
			aria-label="Preview of the {cardName} at {t.toFixed(1)} s"
		>
			{#if mode !== 'card'}<span class="ph">Your {which === 'intro' ? 'first' : 'last'} shot</span>{/if}
			{#each layers as L, i (i)}
				{@const op = cardLive ? layerOpacity({ appear_at: lv(i, 'appear_at', L.appear_at), disappear_at: lv(i, 'disappear_at', L.disappear_at), fade_in: L.fade_in, fade_out: L.fade_out }, cardLive, t) : 1}
				{@const pos = placement(lv(i, 'position', L.position), Number(lv(i, 'x_offset', L.x_offset) ?? 0), Number(lv(i, 'y_offset', L.y_offset) ?? 0), L.kind)}
				{#if L.kind === 'text'}
					{@const f = fontCss(lv(i, 'font', L.font))}
					{@const bg = lv(i, 'background', L.background)}
					<span
						class="tl"
						class:sel={i === selIndex}
						style="{placementStyle(pos, scale)}; font-size: {Number(lv(i, 'size', L.size)) * scale}px; color: {cssColor(lv(i, 'color', L.color))}; font-family: {f.family}; font-weight: {f.weight}; opacity: {op}; {bg?.color ? `background: ${cssColor(bg.color, '#000')}; padding: ${20 * scale}px;` : ''}"
						>{lv(i, 'content', L.content)}</span
					>
				{:else}
					<span
						class="il"
						class:sel={i === selIndex}
						style="{placementStyle(pos, scale)}; height: {Number(lv(i, 'height', L.height) ?? 200) * scale}px; opacity: {op * Number(lv(i, 'opacity', L.opacity) ?? 1)}"
						>Image</span
					>
				{/if}
			{/each}
		</div>
		<label class="scrub">
			<span class="k">At</span>
			<input type="range" min="0" max={dur} step="0.05" bind:value={t} aria-label="Preview the card at this moment" />
			<span class="mono">{t.toFixed(1)} s / {dur.toFixed(1)} s</span>
		</label>
		<p class="hint">The preview follows the renderer's layout; the render has the exact fonts.</p>

		<!-- ---------- the card ---------- -->
		<section class="block" aria-labelledby="{uid}-card-h">
			<div class="block-head">
				<h4 id="{uid}-card-h">The card</h4>
				<button class="btn ghost small" type="button" onclick={removeCard}>Remove the {cardName}</button>
			</div>
			<div class="row">
				<span class="lab" id="{uid}-mode">Show as</span>
				<div class="seg" role="radiogroup" aria-labelledby="{uid}-mode" use:radiogroup>
					<button type="button" role="radio" aria-checked={mode === 'card'} class:on={mode === 'card'} onclick={() => commit('mode', 'card')}
						>A card</button
					>
					<button type="button" role="radio" aria-checked={mode !== 'card'} class:on={mode !== 'card'} onclick={() => commit('mode', 'overlay')}
						>Over the {which === 'intro' ? 'first' : 'last'} shot</button
					>
				</div>
			</div>
			{#if mode === 'card'}
				<div class="row">
					<label class="lab" for="{uid}-bg">Background</label>
					<input
						id="{uid}-bg"
						type="color"
						value={colorInputValue(String(val('bg_color', card.bg_color)), '#0a1628')}
						oninput={(e) => edit('bg_color', e.currentTarget.value)}
						onchange={(e) => commit('bg_color', e.currentTarget.value)}
					/>
					<span class="mono small">{val('bg_color', card.bg_color)}</span>
				</div>
			{/if}
			{#each [{ f: 'duration', label: 'Length', min: 1, max: 12, step: 0.5 }, { f: 'fade_in', label: 'Fade in', min: 0, max: 3, step: 0.1 }, { f: 'fade_out', label: 'Fade out', min: 0, max: 3, step: 0.1 }] as c (c.f)}
				{@const n = secs(val(c.f, card[c.f]), 0)}
				<div class="row">
					<label class="lab" for="{uid}-{c.f}">{c.label}</label>
					<input
						id="{uid}-{c.f}"
						type="range"
						min={c.min}
						max={c.max}
						step={c.step}
						value={n}
						aria-valuetext="{n.toFixed(1)} seconds"
						oninput={(e) => edit(c.f, formatDuration(Number(e.currentTarget.value)))}
						onchange={(e) => commit(c.f, formatDuration(Number(e.currentTarget.value)))}
					/>
					<span class="mono small">{n.toFixed(1)} s</span>
				</div>
			{/each}
		</section>

		<!-- ---------- lines ---------- -->
		<section class="block" aria-labelledby="{uid}-lines-h">
			<div class="block-head">
				<h4 id="{uid}-lines-h">Text and images</h4>
				<button class="btn small" type="button" onclick={addLine}>Add a line of text</button>
			</div>
			{#if layers.length > 0}
				<div class="lines" role="tablist" aria-label="Lines" use:radiogroup>
					{#each layers as L, i (i)}
						<button type="button" role="tab" aria-selected={i === selIndex} class:on={i === selIndex} onclick={() => (selected = i)}
							>{L.kind === 'text' ? short(lv(i, 'content', L.content)) : 'Image'}</button
						>
					{/each}
				</div>
			{/if}

			{#if sel}
				{@const i = selIndex}
				<div class="layer" role="group" aria-label="Line {i + 1}">
					{#if sel.kind === 'text'}
						<div class="row">
							<label class="lab" for="{uid}-text">Text</label>
							<input
								id="{uid}-text"
								class="text"
								type="text"
								value={String(lv(i, 'content', sel.content) ?? '')}
								oninput={(e) => edit(`layers.${i}.content`, e.currentTarget.value)}
								onchange={(e) => commit(`layers.${i}.content`, e.currentTarget.value)}
							/>
						</div>
						<div class="row">
							<label class="lab" for="{uid}-font">Font</label>
							<select
								id="{uid}-font"
								value={String(lv(i, 'font', sel.font) ?? '')}
								onchange={(e) => commit(`layers.${i}.font`, e.currentTarget.value || null)}
							>
								{#each FONTS as f (f.label)}<option value={f.value ?? ''}>{f.label}</option>{/each}
								{#if sel.font && !FONTS.some((f) => f.value === sel.font)}<option value={sel.font}>{sel.font}</option>{/if}
							</select>
						</div>
						<div class="row">
							<label class="lab" for="{uid}-size">Size</label>
							<input
								id="{uid}-size"
								type="range"
								min="16"
								max="240"
								step="2"
								value={Number(lv(i, 'size', sel.size))}
								oninput={(e) => edit(`layers.${i}.size`, Number(e.currentTarget.value))}
								onchange={(e) => commit(`layers.${i}.size`, Number(e.currentTarget.value))}
							/>
							<span class="mono small">{lv(i, 'size', sel.size)} px</span>
						</div>
						<div class="row">
							<label class="lab" for="{uid}-color">Colour</label>
							<input
								id="{uid}-color"
								type="color"
								value={colorInputValue(lv(i, 'color', sel.color))}
								oninput={(e) => edit(`layers.${i}.color`, e.currentTarget.value)}
								onchange={(e) => commit(`layers.${i}.color`, e.currentTarget.value)}
							/>
							<span class="mono small">{lv(i, 'color', sel.color)}</span>
						</div>
					{:else}
						<p class="dim small mono">{sel.path}</p>
						<div class="row">
							<label class="lab" for="{uid}-height">Height</label>
							<input
								id="{uid}-height"
								type="range"
								min="40"
								max="900"
								step="10"
								value={Number(lv(i, 'height', sel.height ?? 200))}
								oninput={(e) => edit(`layers.${i}.height`, Number(e.currentTarget.value))}
								onchange={(e) => commit(`layers.${i}.height`, Number(e.currentTarget.value))}
							/>
							<span class="mono small">{lv(i, 'height', sel.height ?? 200)} px</span>
						</div>
						<div class="row">
							<label class="lab" for="{uid}-opacity">Opacity</label>
							<input
								id="{uid}-opacity"
								type="range"
								min="0"
								max="1"
								step="0.05"
								value={Number(lv(i, 'opacity', sel.opacity ?? 1))}
								oninput={(e) => edit(`layers.${i}.opacity`, Number(e.currentTarget.value))}
								onchange={(e) => commit(`layers.${i}.opacity`, Number(e.currentTarget.value))}
							/>
							<span class="mono small">{Math.round(Number(lv(i, 'opacity', sel.opacity ?? 1)) * 100)}%</span>
						</div>
					{/if}

					<div class="row top">
						<span class="lab" id="{uid}-pos">Position</span>
						<div class="grid" role="radiogroup" aria-labelledby="{uid}-pos" use:radiogroup>
							{#each ANCHOR_GRID as a, gi (gi)}
								{#if a}
									{@const on = lv(i, 'position', sel.position) === a}
									<button
										type="button"
										role="radio"
										aria-checked={on}
										aria-label={ANCHOR_LABELS[a]}
										class:on
										onclick={() => {
											if (on) return;
											// A new anchor starts without the old nudges, so the line can't land off-frame.
											for (const f of ['position', 'x_offset', 'y_offset']) edit(`layers.${i}.${f}`, f === 'position' ? a : 0);
											save(
												[
													{ key: `${which}.layers.${i}.position`, value: a },
													{ key: `${which}.layers.${i}.x_offset`, value: 0 },
													{ key: `${which}.layers.${i}.y_offset`, value: 0 }
												],
												`Moved to ${ANCHOR_LABELS[a].toLowerCase()}.`
											);
										}}><span aria-hidden="true"></span></button
									>
								{:else}
									<span class="none" aria-hidden="true"></span>
								{/if}
							{/each}
						</div>
						<div class="nudge">
							{#each [{ f: 'x_offset', label: 'Left / right', max: 800 }, { f: 'y_offset', label: 'Up / down', max: 500 }] as c (c.f)}
								<label class="nudge-row">
									<span class="k">{c.label}</span>
									<input
										type="range"
										min={-c.max}
										max={c.max}
										step="5"
										value={Number(lv(i, c.f, sel[c.f] ?? 0))}
										oninput={(e) => edit(`layers.${i}.${c.f}`, Number(e.currentTarget.value))}
										onchange={(e) => commit(`layers.${i}.${c.f}`, Number(e.currentTarget.value))}
									/>
									<span class="mono small">{lv(i, c.f, sel[c.f] ?? 0)} px</span>
								</label>
							{/each}
							{#if Number(sel.x_offset ?? 0) !== 0 || Number(sel.y_offset ?? 0) !== 0}
								<button
									class="btn ghost small"
									type="button"
									onclick={() =>
										save(
											[
												{ key: `${which}.layers.${i}.x_offset`, value: 0 },
												{ key: `${which}.layers.${i}.y_offset`, value: 0 }
											],
											'Back on its anchor.'
										)}>Back on the anchor</button
								>
							{/if}
						</div>
					</div>

					{#each [{ f: 'appear_at', label: 'Appears at', dflt: 0 }, { f: 'disappear_at', label: 'Disappears at', dflt: dur }] as c (c.f)}
						{@const n = Math.min(dur, secs(lv(i, c.f, sel[c.f]), c.dflt))}
						<div class="row">
							<label class="lab" for="{uid}-{c.f}">{c.label}</label>
							<input
								id="{uid}-{c.f}"
								type="range"
								min="0"
								max={dur}
								step="0.1"
								value={n}
								aria-valuetext="{n.toFixed(1)} seconds"
								oninput={(e) => {
									edit(`layers.${i}.${c.f}`, formatDuration(Number(e.currentTarget.value)));
									t = Number(e.currentTarget.value) + (c.f === 'appear_at' ? 0.3 : -0.3);
								}}
								onchange={(e) => {
									const v = Number(e.currentTarget.value);
									// The edges are the defaults: leave them unset.
									const none = c.f === 'appear_at' ? v <= 0 : v >= dur;
									commit(`layers.${i}.${c.f}`, none ? null : formatDuration(v));
								}}
							/>
							<span class="mono small">{n.toFixed(1)} s</span>
						</div>
					{/each}

					{#if sel.kind === 'text'}
						{@const bg = lv(i, 'background', sel.background)}
						{@const box = boxColor(bg?.color)}
						<div class="row">
							<label class="lab" for="{uid}-box">Box behind</label>
							<input
								id="{uid}-box"
								type="checkbox"
								role="switch"
								checked={!!bg}
								onchange={(e) => commit(`layers.${i}.background`, e.currentTarget.checked ? { color: '#00000099' } : null)}
							/>
							{#if bg}
								<input
									type="color"
									aria-label="Box colour"
									value={box.rgb}
									onchange={(e) => commit(`layers.${i}.background`, { ...bg, color: boxValue(e.currentTarget.value, box.alpha) })}
								/>
								<input
									class="alpha"
									type="range"
									min="0"
									max="1"
									step="0.05"
									aria-label="Box opacity"
									value={box.alpha}
									oninput={(e) => edit(`layers.${i}.background`, { ...bg, color: boxValue(box.rgb, Number(e.currentTarget.value)) })}
									onchange={(e) => commit(`layers.${i}.background`, { ...bg, color: boxValue(box.rgb, Number(e.currentTarget.value)) })}
								/>
								<span class="mono small">{Math.round(box.alpha * 100)}%</span>
							{/if}
						</div>
					{/if}

					<button class="btn ghost small remove" type="button" onclick={() => removeLine(i)}
						>Remove this {sel.kind === 'text' ? 'line' : 'image'}</button
					>
				</div>
			{/if}
		</section>
	{/if}
	<p class="sr-only" role="status">{note}</p>
</div>

<style>
	.titles {
		display: flex;
		flex-direction: column;
		gap: 12px;
	}

	.which,
	.lines,
	.seg {
		display: flex;
		flex-wrap: wrap;
		gap: 6px;
	}

	.which button,
	.lines button,
	.seg button {
		border: 1px solid var(--line-strong);
		background: transparent;
		color: var(--text-dim);
		border-radius: 999px;
		padding: 5px 13px;
		font-weight: 600;
		font-size: var(--fs-sm);
		cursor: pointer;
		max-width: 220px;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.which button.on,
	.lines button.on,
	.seg button.on {
		background: var(--text);
		color: var(--ink);
		border-color: var(--text);
	}

	.empty {
		display: flex;
		flex-direction: column;
		align-items: flex-start;
		gap: 10px;
		padding: 14px;
		border: 1px dashed var(--line-strong);
		border-radius: var(--r-md);
	}

	.empty p {
		margin: 0;
		color: var(--text-dim);
	}

	.preview {
		position: relative;
		width: 100%;
		overflow: hidden;
		border-radius: var(--r-md);
		border: 1px solid var(--line);
		background: #0a1628;
	}

	.preview.overlay {
		background:
			linear-gradient(160deg, rgba(56, 214, 255, 0.35), rgba(255, 122, 69, 0.3)),
			repeating-linear-gradient(45deg, #1b2230 0 14px, #161c28 14px 28px);
	}

	.ph {
		position: absolute;
		left: 10px;
		top: 8px;
		font-size: 11px;
		color: rgba(255, 255, 255, 0.6);
	}

	.tl,
	.il {
		position: absolute;
		white-space: pre;
		line-height: 1.15;
		transition: opacity 120ms linear;
	}

	.il {
		display: grid;
		place-items: center;
		aspect-ratio: 1;
		border: 1px dashed rgba(255, 255, 255, 0.6);
		color: rgba(255, 255, 255, 0.7);
		font-size: 11px;
	}

	.tl.sel,
	.il.sel {
		outline: 1px dashed rgba(255, 255, 255, 0.5);
		outline-offset: 3px;
	}

	.scrub,
	.nudge-row {
		display: grid;
		grid-template-columns: auto minmax(0, 1fr) auto;
		gap: 10px;
		align-items: center;
	}

	.k {
		font-size: var(--fs-xs);
		color: var(--text-faint);
		font-weight: 700;
		text-transform: uppercase;
		letter-spacing: 0.06em;
	}

	.hint {
		margin: -4px 0 0;
		font-size: var(--fs-xs);
		color: var(--text-faint);
	}

	.block {
		display: flex;
		flex-direction: column;
		gap: 10px;
		padding: 12px 0;
		border-top: 1px solid var(--line);
	}

	.block-head {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 10px;
	}

	h4 {
		margin: 0;
		font-size: var(--fs-sm);
	}

	.row {
		display: flex;
		align-items: center;
		flex-wrap: wrap;
		gap: 8px 12px;
	}

	.row.top {
		align-items: flex-start;
	}

	.lab {
		width: 108px;
		font-weight: 600;
		font-size: var(--fs-sm);
	}

	.row input[type='range'] {
		flex: 1;
		min-width: 140px;
		accent-color: var(--magenta);
	}

	.row input.alpha {
		flex: 0 1 140px;
		min-width: 100px;
	}

	.text,
	select {
		flex: 1;
		min-width: 0;
		padding: 7px 10px;
		border-radius: var(--r-md);
		border: 1px solid var(--line-strong);
		background: rgba(0, 0, 0, 0.25);
		color: var(--text);
		font: inherit;
	}

	input[type='color'] {
		width: 40px;
		height: 28px;
		padding: 0;
		border: 1px solid var(--line-strong);
		border-radius: 6px;
		background: transparent;
	}

	input[type='checkbox'] {
		width: 18px;
		height: 18px;
		accent-color: var(--magenta);
	}

	.small {
		font-size: var(--fs-xs);
		color: var(--text-dim);
	}

	.layer {
		display: flex;
		flex-direction: column;
		gap: 10px;
		padding: 12px;
		border-radius: var(--r-md);
		border: 1px solid var(--line);
		background: rgba(255, 255, 255, 0.02);
	}

	.grid {
		display: grid;
		grid-template-columns: repeat(3, 26px);
		gap: 4px;
	}

	.grid button,
	.grid .none {
		width: 26px;
		height: 20px;
		border-radius: 4px;
	}

	.grid button {
		border: 1px solid var(--line-strong);
		background: transparent;
		cursor: pointer;
		display: grid;
		place-items: center;
	}

	.grid button span {
		width: 6px;
		height: 6px;
		border-radius: 50%;
		background: var(--text-faint);
	}

	.grid button.on {
		border-color: var(--magenta);
		background: color-mix(in srgb, var(--magenta) 20%, transparent);
	}

	.grid button.on span {
		background: var(--magenta);
	}

	.nudge {
		flex: 1;
		min-width: 220px;
		display: flex;
		flex-direction: column;
		gap: 6px;
	}

	.remove {
		align-self: flex-start;
	}
</style>
