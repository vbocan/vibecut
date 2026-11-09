<script lang="ts">
	import { untrack } from 'svelte';
	import { appState as s } from '$lib/state.svelte';
	import { outputFileUrl, type JobListEntry, type PlatformPreset } from '$lib/api';
	import { CONTROLS, GROUPS, formatDuration, getPath, keyLabel, parseDuration, sameValue, showValue, type Control, type Group } from '$lib/finetune';
	import { keepFocus } from '$lib/focus';
	import { radiogroup } from '$lib/radiogroup';

	let { open = $bindable(false) }: { open?: boolean } = $props();
	let dialog: HTMLDialogElement | undefined = $state();

	$effect(() => {
		if (!dialog) return;
		if (open && !dialog.open) dialog.showModal();
		if (!open && dialog.open) dialog.close();
	});

	// ---- Fine-tune: controls grouped by goal, each writing one key of
	// the project's TOML in place (comments and order kept), with the
	// vibe's setting marked and a Reset; the edit is re-planned after each
	// change so the strip shows what a render would cut.
	let group = $state('rhythm');
	/** A slider's value while it is being dragged (committed on release). */
	let live = $state<Record<string, unknown>>({});
	let tuneNote = $state('');
	const currentGroup = $derived(GROUPS.find((g) => g.id === group) ?? GROUPS[0]);
	const cfgJson = $derived(s.configStructured as unknown);

	$effect(() => {
		if (open) untrack(() => void s.openFineTune());
	});

	function value(key: string): unknown {
		return getPath(cfgJson, key);
	}

	function defaultOf(key: string): unknown {
		return s.tuneDefaults ? getPath(s.tuneDefaults.defaults, key) : undefined;
	}

	/** Music controls only with a song; title controls only when the card is there. */
	function shown(c: Control): boolean {
		if (c.needs === 'soundtrack') return !!s.configStructured?.soundtrack;
		if (c.needs === 'intro' || c.needs === 'outro') return value(c.key) !== undefined;
		return true;
	}

	function changedIn(g: Group): number {
		const keys = new Set((s.tuneDefaults?.changes ?? []).map((c) => c.key));
		return g.controls.filter((c) => keys.has(c.key)).length;
	}

	async function commit(c: Control, raw: unknown) {
		const v = c.kind === 'seconds' ? formatDuration(Number(raw)) : raw;
		delete live[c.key];
		if (sameValue(c, value(c.key), v)) return;
		tuneNote = `${c.label}: ${showValue(c, v)}`;
		if (!(await s.tune([{ key: c.key, value: v }]))) tuneNote = s.tuneError ?? '';
	}

	async function reset(key: string, def: unknown) {
		if (s.tuneSaving) return;
		if (await s.tune([{ key, value: def ?? null }])) tuneNote = `${keyLabel(key)} is back to the vibe's setting.`;
	}

	async function resetAll() {
		const ch = s.tuneDefaults?.changes ?? [];
		if (s.tuneSaving || ch.length === 0) return;
		if (await s.tune(ch.map((c) => ({ key: c.key, value: c.default ?? null }))))
			tuneNote = `Back to ${s.tuneDefaults?.vibe_name ?? 'the vibe'} as it comes.`;
	}

	function brief(v: unknown): string {
		if (v === null || v === undefined) return '—';
		return typeof v === 'object' ? JSON.stringify(v).slice(0, 40) : String(v);
	}

	const changeCount = $derived.by(() => {
		const n = s.tuneDefaults?.changes.length ?? 0;
		return n === 0 ? 'no changes' : `${n} change${n === 1 ? '' : 's'}`;
	});

	const shots = $derived(
		(s.tunePlan?.timeline.clips ?? []).map((c) => Math.max(0.05, c.cut_out_secs - c.cut_in_secs))
	);
	const planNote = $derived.by(() => {
		const p = s.tunePlan;
		if (!p) return '';
		const secs = Math.round(p.duration_secs);
		const song = p.song ? ` · ${Math.round(p.song.tempo_bpm)} BPM` : '';
		return `${p.timeline.clips.length} shots · ${Math.floor(secs / 60)}:${String(secs % 60).padStart(2, '0')}${song}`;
	});

	// ---- Renders list: recent jobs (renders and platform versions),
	// refreshed whenever it's shown and whenever a render starts or ends
	// while it is.
	let rendersOpen = $state(false);
	$effect(() => {
		void s.renderMode;
		if (open && rendersOpen) void s.loadJobs();
	});

	const JOB_LABEL: Record<JobListEntry['state'], string> = {
		queued: 'Queued',
		running: 'Rendering',
		done: 'Done',
		failed: 'Failed',
		cancelled: 'Stopped'
	};

	const KIND_LABEL: Record<JobListEntry['kind'], string> = { render: 'Render', reformat: 'Reformat' };

	const PRESET_LABEL: Record<PlatformPreset, string> = {
		youtube: 'YouTube',
		reels: 'Reels',
		tiktok: 'TikTok',
		shorts: 'Shorts',
		square: 'Square',
		portrait: 'Portrait',
		whatsapp: 'WhatsApp',
		master: 'Master'
	};

	function stateLabel(j: JobListEntry): string {
		if (j.state === 'running' && j.kind === 'reformat') return 'Making';
		return JOB_LABEL[j.state];
	}

	/** The finished file: a render's kept version, a reformat's version. */
	function jobFile(j: JobListEntry): string | undefined {
		if (j.state !== 'done') return undefined;
		return j.kind === 'render' ? (j.version_path ?? j.output_path) : j.output_path;
	}

	/** "proposal_coastline_…" for a render, "Reels version of coast.mp4" for a reformat. */
	function jobTitle(j: JobListEntry): string {
		if (j.kind === 'reformat') {
			const from = j.source_path.split('/').pop() ?? j.source_path;
			const fit = j.fit === 'smart' ? 'follows the action' : j.fit;
			return `${PRESET_LABEL[j.preset] ?? j.preset}${fit ? ` (${fit})` : ''} version of ${from}`;
		}
		return projectName(j.config_path);
	}

	/** A finished render that was a quick draft. */
	function isDraft(j: JobListEntry): boolean {
		return j.kind === 'render' && j.state === 'done' && j.quality === 'draft';
	}

	function isFinished(j: JobListEntry): boolean {
		return j.state === 'done' || j.state === 'failed' || j.state === 'cancelled';
	}

	function clockTime(iso: string): string {
		const d = new Date(iso);
		return Number.isNaN(d.getTime()) ? '' : d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
	}

	function took(j: JobListEntry): string {
		if (!('elapsed_ms' in j) || (j.state === 'cancelled' && !j.started_at)) return '';
		const secs = Math.round(j.elapsed_ms / 1000);
		return ` · ${secs < 60 ? `${secs} s` : `${Math.floor(secs / 60)} min ${secs % 60} s`}`;
	}

	/** `/projects/proposal_coastline_20261108T101500.toml` → `proposal_coastline_20261108T101500`. */
	function projectName(path: string): string {
		return (path.split('/').pop() ?? path).replace(/\.toml$/, '') || '—';
	}

	function belongsHere(j: JobListEntry): boolean {
		if (j.kind !== 'render') return false;
		const file = jobFile(j);
		return (
			!!file &&
			(j.config_path === s.proposeResult?.config_path || s.renderHistory.some((v) => v.output_path === file))
		);
	}

	let refreshBtn: HTMLButtonElement | undefined = $state();
	/** What the last action in the list did, for screen readers. */
	let jobsNote = $state('');

	async function stopJob(j: JobListEntry, what: string) {
		if (j.cancel_requested) return;
		jobsNote = `Stopping ${what}…`;
		await s.stopJob(j.job_id);
		const now = s.jobs.find((x) => x.job_id === j.job_id);
		jobsNote = now && isFinished(now) ? `${what}: ${stateLabel(now).toLowerCase()}.` : `Stopping ${what}.`;
	}

	/** Where the focus goes when a row's Stop button goes away. */
	function afterStop(id: string): HTMLElement | null {
		const row = document.querySelector<HTMLElement>(`.renders li[data-job="${CSS.escape(id)}"]`);
		return row?.querySelector<HTMLElement>('.btn') ?? refreshBtn ?? null;
	}

	async function openJob(j: JobListEntry) {
		if (await s.openJob(j)) open = false;
	}

	async function render() {
		if (s.rendering || s.tuneSaving) return;
		s.configError = null;
		if (s.configDirty) await s.saveTomlFromTune();
		if (!s.configError) {
			open = false;
			void s.startRenderAsync();
		}
	}
</script>

<dialog bind:this={dialog} class="drawer" aria-labelledby="pro-h" onclose={() => (open = false)}>
	<header class="ft-head">
		<div class="ft-title">
			<p class="eyebrow">Fine-tune</p>
			<h2 id="pro-h">{s.proposeResult?.title || s.title || s.folderLabel || 'Your video'}</h2>
			{#if s.tuneDefaults}
				<p class="based">Based on <strong>{s.tuneDefaults.vibe_name}</strong> · {changeCount}</p>
			{/if}
		</div>
		<div class="ft-actions">
			{#if s.proposeResult}
				<button
					class="btn primary"
					type="button"
					disabled={s.tuneSaving || s.savingConfig || s.rendering}
					title={s.rendering ? 'Wait for the current render to finish' : undefined}
					onclick={render}
				>
					{s.rendering ? 'Rendering…' : s.fastPreviews ? 'Render draft' : 'Render'}
				</button>
			{/if}
			<button class="btn ghost" type="button" aria-label="Close Fine-tune" onclick={() => (open = false)}>✕</button>
		</div>
	</header>

	{#if s.proposeResult}
		<div class="ft-grid">
			<div class="ft-left">
				{#if s.outputPath}
					<!-- svelte-ignore a11y_media_has_caption -->
					<video class="ft-video" src="{outputFileUrl(s.outputPath)}#t=1" controls preload="metadata"></video>
				{/if}

				<section class="card plan" aria-labelledby="plan-h">
					<div class="card-head">
						<h3 id="plan-h" class="eyebrow">The edit as set now</h3>
						<span class="dim small" role="status">{s.tunePlanning ? 'Planning…' : planNote}</span>
					</div>
					{#if s.tunePlan}
						<div class="cutstrip" role="img" aria-label="{planNote}: one block per shot, as long as the shot">
							{#each shots as w, i (i)}<span style="flex-grow: {w}"></span>{/each}
						</div>
						<p class="dim small">Planned from your settings without rendering. Render to see it.</p>
					{:else if !s.tunePlanning}
						<p class="dim small">The plan appears here after the first change.</p>
					{/if}
				</section>

				<section class="card" aria-labelledby="changes-h">
					<div class="card-head">
						<h3 id="changes-h" class="eyebrow">Changes from {s.tuneDefaults?.vibe_name ?? 'the vibe'}</h3>
						{#if (s.tuneDefaults?.changes.length ?? 0) > 0}
							<button class="btn ghost small" type="button" aria-disabled={s.tuneSaving} onclick={resetAll}
								>Reset all</button
							>
						{/if}
					</div>
					{#if s.tuneDefaults && s.tuneDefaults.changes.length > 0}
						<ul class="changes">
							{#each s.tuneDefaults.changes as c (c.key)}
								{@const ctl = CONTROLS.get(c.key)}
								<li>
									<span class="c-label">{keyLabel(c.key)}</span>
									<span class="c-val"
										>{ctl ? showValue(ctl, c.value) : brief(c.value)}
										<span class="dim">(vibe: {ctl ? showValue(ctl, c.default) : brief(c.default)})</span></span
									>
									<button
										class="btn ghost small"
										type="button"
										aria-label="Reset {keyLabel(c.key)} to the vibe's setting"
										aria-disabled={s.tuneSaving}
										onclick={() => reset(c.key, c.default)}>Reset</button
									>
								</li>
							{/each}
						</ul>
					{:else}
						<p class="dim small">Nothing changed yet: this is {s.tuneDefaults?.vibe_name ?? 'the vibe'} as it comes.</p>
					{/if}
				</section>
			</div>

			<div class="ft-right">
				<div class="tabs" role="tablist" aria-label="Settings view" use:radiogroup>
					<button
						type="button"
						role="tab"
						id="pro-tab-guided"
						aria-controls="pro-panel"
						aria-selected={s.configMode === 'guided'}
						class:active={s.configMode === 'guided'}
						onclick={() => (s.configMode = 'guided')}>Controls</button
					>
					<button
						type="button"
						role="tab"
						id="pro-tab-raw"
						aria-controls="pro-panel"
						aria-selected={s.configMode === 'raw'}
						class:active={s.configMode === 'raw'}
						onclick={() => (s.configMode = 'raw')}>TOML</button
					>
				</div>

				<div class="body" role="tabpanel" id="pro-panel" aria-labelledby="pro-tab-{s.configMode}">
					{#if s.configMode === 'guided'}
						<div class="groups" role="tablist" aria-label="Settings groups" use:radiogroup>
							{#each GROUPS as g (g.id)}
								<button
									type="button"
									role="tab"
									id="group-tab-{g.id}"
									aria-controls="group-panel"
									aria-selected={group === g.id}
									class:active={group === g.id}
									onclick={() => (group = g.id)}
									>{g.label}{#if changedIn(g) > 0}<span class="dot" aria-hidden="true"></span><span class="sr-only"
											>, {changedIn(g)} changed</span
										>{/if}</button
								>
							{/each}
						</div>
						<div class="group" role="tabpanel" id="group-panel" aria-labelledby="group-tab-{currentGroup.id}">
							<p class="dim">{currentGroup.blurb}</p>
							{#if currentGroup.id === 'music'}
								<div class="song-row">
									<span>♪ <strong>{s.effectiveTrack?.title ?? 'Original sound'}</strong></span>
									<button class="btn small" type="button" aria-haspopup="dialog" onclick={() => (s.musicSheet = 'songs')}
										>Change the music</button
									>
								</div>
							{/if}
							{#each currentGroup.controls.filter(shown) as c (c.key)}
								{@const cur = value(c.key)}
								{@const def = defaultOf(c.key)}
								{@const changed = def !== undefined && !sameValue(c, cur, def)}
								<div class="ctl" class:changed>
									<div class="ctl-row">
										<label class="ctl-label" for="ctl-{c.key}">{c.label}</label>
										<span class="ctl-val mono">{showValue(c, live[c.key] ?? cur)}</span>
									</div>
									{#if c.kind === 'range' || c.kind === 'seconds'}
										{@const n = c.kind === 'seconds' ? parseDuration(live[c.key] ?? cur) : Number(live[c.key] ?? cur)}
										<input
											id="ctl-{c.key}"
											type="range"
											min={c.min}
											max={c.max}
											step={c.step}
											value={Number.isFinite(n) ? n : c.min}
											aria-describedby="help-{c.key}"
											aria-valuetext={showValue(c, c.kind === 'seconds' ? formatDuration(n) : n)}
											oninput={(e) => {
												const v = Number(e.currentTarget.value);
												live[c.key] = c.kind === 'seconds' ? formatDuration(v) : v;
											}}
											onchange={(e) => commit(c, Number(e.currentTarget.value))}
										/>
										{#if def !== undefined}
											{@const dn = c.kind === 'seconds' ? parseDuration(def) : Number(def)}
											{#if Number.isFinite(dn) && c.max !== undefined && c.min !== undefined}
												<span class="defmark" style="left: {((dn - c.min) / (c.max - c.min)) * 100}%" aria-hidden="true"></span>
											{/if}
										{/if}
									{:else if c.kind === 'choice'}
										<div class="choices" role="radiogroup" id="ctl-{c.key}" aria-label={c.label} aria-describedby="help-{c.key}" use:radiogroup>
											{#each c.choices ?? [] as ch (ch.value)}
												<button
													type="button"
													role="radio"
													aria-checked={String(cur) === String(ch.value)}
													class:on={String(cur) === String(ch.value)}
													onclick={() => commit(c, ch.value)}>{ch.label}</button
												>
											{/each}
										</div>
									{:else if c.kind === 'toggle'}
										<input
											id="ctl-{c.key}"
											type="checkbox"
											role="switch"
											checked={!!cur}
											aria-describedby="help-{c.key}"
											onchange={(e) => commit(c, e.currentTarget.checked)}
										/>
									{:else}
										<input
											id="ctl-{c.key}"
											class="text"
											type="text"
											value={String(cur ?? '')}
											aria-describedby="help-{c.key}"
											onchange={(e) => commit(c, e.currentTarget.value)}
										/>
									{/if}
									<div class="ctl-row foot">
										<span class="help" id="help-{c.key}"
											>{c.help}{#if def !== undefined}{' '}<span class="vibe-def"
													>{s.tuneDefaults?.vibe_name}: {showValue(c, def)}.</span
												>{/if}</span
										>
										{#if changed}
											<button
												class="btn ghost small"
												type="button"
												aria-label="Reset {c.label} to {showValue(c, def)}"
												aria-disabled={s.tuneSaving}
												onclick={() => reset(c.key, def)}>Reset</button
											>
										{/if}
									</div>
								</div>
							{/each}
						</div>
					{:else}
						<p class="dim small">
							The project file itself. Edits here keep your comments; settings the Controls don't show (extra
							clips, photos, several cameras, more outputs) are set here. CONFIG.md describes every key.
						</p>
						<label class="sr-only" for="raw-toml">Project TOML</label>
						<textarea
							id="raw-toml"
							class="raw mono"
							value={s.configToml}
							oninput={(e) => s.setConfigToml((e.target as HTMLTextAreaElement).value)}
							spellcheck="false"
						></textarea>
						<div class="toml-foot">
							<span class="state">{s.configDirty ? 'Unsaved changes' : 'Saved'}</span>
							<button class="btn" type="button" disabled={!s.configDirty || s.savingConfig} onclick={() => s.saveTomlFromTune()}
								>{s.savingConfig ? 'Saving…' : 'Save TOML'}</button
							>
						</div>
						{#if s.configError}<p class="error" role="alert">{s.configError}</p>{/if}
					{/if}
					{#if s.tuneError}<p class="error" role="alert">{s.tuneError}</p>{/if}
					<p class="sr-only" role="status">{tuneNote}</p>
				</div>
			</div>
		</div>

		<details class="logs renders" bind:open={rendersOpen}>
			<summary>Renders</summary>
			<!-- Buttons here stay focusable while busy (aria-disabled), and a
			     Stop whose row turns into "Stopped" hands the focus to that
			     row's next control, else to Refresh. -->
			<button
				class="btn ghost small"
				type="button"
				aria-disabled={s.loadingJobs}
				bind:this={refreshBtn}
				onclick={() => {
					if (!s.loadingJobs) void s.loadJobs();
				}}
			>
				{s.loadingJobs ? 'Loading…' : 'Refresh'}
			</button>
			<p class="sr-only" role="status">{jobsNote}</p>
			{#if s.jobsError}<p class="error" role="alert">{s.jobsError}</p>{/if}
			<ul aria-label="Recent renders and platform versions" aria-busy={s.loadingJobs}>
				{#each s.jobs as j (j.job_id)}
					{@const file = jobFile(j)}
					{@const what = jobTitle(j)}
					<li data-job={j.job_id}>
						<span class="job-state {j.state}">{stateLabel(j)}{j.cancel_requested && !isFinished(j) ? ' · stopping' : ''}</span>
						<span class="kind {j.kind}" class:draft={isDraft(j)}>{isDraft(j) ? 'Draft' : (KIND_LABEL[j.kind] ?? j.kind)}</span>
						<span class="when mono">{clockTime(j.submitted_at)}{took(j)}</span>
						<span class="what" title={j.kind === 'reformat' ? j.output_path : j.config_path}>{jobTitle(j)}</span>
						{#if j.state === 'queued' || j.state === 'running'}
							<button
								class="btn ghost small"
								type="button"
								aria-label="Stop {what}"
								aria-disabled={j.cancel_requested}
								use:keepFocus={() => afterStop(j.job_id)}
								onclick={() => stopJob(j, what)}>Stop</button
							>
						{:else if file}
							{#if belongsHere(j)}
								<button
									class="btn ghost small"
									type="button"
									aria-label="Open {what} in the Studio"
									aria-disabled={s.rendering}
									onclick={() => {
										if (!s.rendering) void openJob(j);
									}}>Open</button
								>
							{:else}
								<a
									class="btn ghost small"
									href={outputFileUrl(file)}
									target="_blank"
									rel="noopener"
									aria-label="Watch {what} (opens in a new tab)">Watch</a
								>
							{/if}
						{/if}
					</li>
				{:else}
					<li class="dim">{s.loadingJobs ? 'Loading…' : 'No renders yet.'}</li>
				{/each}
			</ul>
		</details>

		<details class="logs">
			<summary>Session logs</summary>
			<button class="btn ghost small" type="button" onclick={() => s.loadSessions()} disabled={s.loadingSessions}>
				{s.loadingSessions ? 'Loading…' : 'Refresh'}
			</button>
			<ul>
				{#each s.sessions as f (f.path)}
					<li class="mono">{f.filename} <span>{(f.size_bytes / 1024).toFixed(1)} KB</span></li>
				{:else}
					<li class="dim">No session logs yet.</li>
				{/each}
			</ul>
		</details>
	{:else}
		<p class="dim">Make a video first — its settings appear here.</p>
	{/if}
</dialog>

<style>
	.drawer {
		margin: 0;
		height: 100dvh;
		max-height: 100dvh;
		width: 100vw;
		max-width: 100vw;
		padding: 22px 24px;
		border: none;
		border-left: 1px solid var(--line-strong);
		background: rgba(11, 13, 19, 0.96);
		color: var(--text);
		display: flex;
		flex-direction: column;
		gap: 12px;
		overflow-y: auto;
	}

	.drawer:not([open]) {
		display: none;
	}

	.drawer[open] {
		animation: slide var(--dur-3) var(--ease-out) both;
	}

	.drawer::backdrop {
		background: rgba(3, 4, 7, 0.6);
		backdrop-filter: blur(6px);
	}

	header {
		display: flex;
		align-items: flex-start;
		justify-content: space-between;
	}

	h2 {
		margin: 2px 0 0;
		font-size: var(--fs-2xl);
		letter-spacing: -0.03em;
	}

	.eyebrow {
		margin: 0;
	}

	.path {
		margin: 0;
		font-size: var(--fs-xs);
		color: var(--text-faint);
		overflow-wrap: anywhere;
	}

	.tabs {
		display: inline-flex;
		gap: 4px;
		padding: 4px;
		border-radius: 999px;
		border: 1px solid var(--line);
		width: fit-content;
	}

	.tabs button {
		border: none;
		background: transparent;
		color: var(--text-dim);
		padding: 6px 14px;
		border-radius: 999px;
		font-weight: 600;
		cursor: pointer;
	}

	.tabs button.active {
		background: var(--text);
		color: var(--ink);
	}

	.body {
		flex: 1;
	}

	.raw {
		width: 100%;
		min-height: 55vh;
		background: #05060a;
		border: 1px solid var(--line);
		border-radius: var(--r-md);
		padding: 14px;
		font-size: 12.5px;
		line-height: 1.55;
		resize: vertical;
	}


	.warn {
		color: var(--warn);
		font-size: var(--fs-xs);
	}

	.state {
		color: var(--text-faint);
		font-size: var(--fs-sm);
	}

	.logs summary {
		cursor: pointer;
		color: var(--text-dim);
	}

	.logs ul {
		list-style: none;
		padding: 0;
		margin: 8px 0 0;
		font-size: var(--fs-xs);
		color: var(--text-dim);
	}

	.logs li span {
		color: var(--text-faint);
		margin-left: 8px;
	}

	.renders li {
		display: flex;
		align-items: center;
		gap: 4px;
		padding: 4px 0;
		border-bottom: 1px solid var(--line);
	}

	.renders li .job-state {
		margin-left: 0;
		min-width: 82px;
		font-weight: 700;
	}

	.renders .job-state.running,
	.renders .job-state.queued {
		color: var(--cyan);
	}

	.renders .job-state.done {
		color: var(--good);
	}

	.renders .job-state.failed {
		color: var(--bad);
	}

	.renders .kind {
		font-size: var(--fs-xs);
		padding: 1px 6px;
		border-radius: 999px;
		border: 1px solid var(--line-strong);
		color: var(--text-dim);
	}

	.renders .kind.reformat,
	.renders .kind.draft {
		color: var(--cyan);
	}

	/* ---- Fast previews switch ---- */

	.renders .what {
		flex: 1;
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.renders li .small {
		margin-top: 0;
	}

	.small {
		padding: 4px 10px;
		font-size: var(--fs-xs);
		margin-top: 8px;
	}

	.dim {
		color: var(--text-faint);
	}

	.error {
		color: var(--bad);
	}

	@keyframes slide {
		from {
			transform: translateX(40px);
			opacity: 0;
		}
	}

	/* ---------- Fine-tune page ---------- */
	.ft-head {
		display: flex;
		align-items: flex-start;
		justify-content: space-between;
		gap: 16px;
		max-width: 1500px;
		width: 100%;
		margin: 0 auto;
	}

	.ft-actions {
		display: flex;
		gap: 10px;
		align-items: center;
	}

	.based {
		margin: 4px 0 0;
		color: var(--text-dim);
		font-size: var(--fs-sm);
	}

	.ft-grid {
		display: grid;
		grid-template-columns: minmax(0, 1fr) minmax(0, 1.05fr);
		gap: 22px;
		max-width: 1500px;
		width: 100%;
		margin: 0 auto;
		align-items: start;
	}

	.ft-left {
		display: flex;
		flex-direction: column;
		gap: 14px;
		position: sticky;
		top: 0;
	}

	.ft-right {
		display: flex;
		flex-direction: column;
		gap: 12px;
		min-width: 0;
	}

	.ft-video {
		width: 100%;
		aspect-ratio: 16 / 9;
		border-radius: var(--r-lg);
		background: #000;
		border: 1px solid var(--line);
	}

	.card {
		display: flex;
		flex-direction: column;
		gap: 10px;
		padding: 14px 16px;
		border-radius: var(--r-md);
		border: 1px solid var(--line);
		background: rgba(255, 255, 255, 0.025);
	}

	.card-head {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 10px;
	}

	.card-head h3 {
		margin: 0;
	}

	.small {
		font-size: var(--fs-xs);
		margin: 0;
	}

	.cutstrip {
		display: flex;
		gap: 3px;
		height: 22px;
	}

	.cutstrip span {
		flex-basis: 0;
		min-width: 3px;
		border-radius: 4px;
		background: var(--magenta);
		opacity: 0.85;
	}

	.cutstrip span:nth-child(3n) {
		opacity: 0.55;
	}

	.cutstrip span:nth-child(3n + 1) {
		background: var(--cyan);
	}

	.changes {
		list-style: none;
		margin: 0;
		padding: 0;
		display: flex;
		flex-direction: column;
		gap: 6px;
		max-height: 260px;
		overflow-y: auto;
	}

	.changes li {
		display: grid;
		grid-template-columns: minmax(0, 1fr) auto;
		gap: 2px 10px;
		align-items: center;
		font-size: var(--fs-sm);
	}

	.c-label {
		font-weight: 600;
	}

	.c-val {
		grid-column: 1;
		color: var(--text);
		font-size: var(--fs-xs);
	}

	.changes li .btn {
		grid-column: 2;
		grid-row: 1 / span 2;
	}

	.groups {
		display: flex;
		flex-wrap: wrap;
		gap: 6px;
	}

	.groups button {
		position: relative;
		border: 1px solid var(--line);
		background: transparent;
		color: var(--text-dim);
		border-radius: var(--r-md);
		padding: 7px 13px;
		font-weight: 600;
		font-size: var(--fs-sm);
		cursor: pointer;
	}

	.groups button.active {
		color: var(--text);
		border-color: var(--magenta);
		background: color-mix(in srgb, var(--magenta) 12%, transparent);
	}

	.groups .dot {
		display: inline-block;
		width: 7px;
		height: 7px;
		margin-left: 6px;
		border-radius: 50%;
		background: #ffc94a;
		vertical-align: middle;
	}

	.group {
		display: flex;
		flex-direction: column;
		gap: 4px;
		margin-top: 8px;
	}

	.group > .dim {
		margin: 0 0 6px;
		font-size: var(--fs-sm);
	}

	.song-row {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 10px;
		padding: 10px 12px;
		border-radius: var(--r-md);
		border: 1px solid var(--line);
		margin-bottom: 6px;
	}

	.ctl {
		position: relative;
		display: flex;
		flex-direction: column;
		gap: 6px;
		padding: 12px 0;
		border-bottom: 1px solid var(--line);
	}

	.ctl.changed .ctl-label::after {
		content: ' · changed';
		color: #ffc94a;
		font-weight: 600;
		font-size: var(--fs-xs);
	}

	.ctl-row {
		display: flex;
		align-items: baseline;
		justify-content: space-between;
		gap: 10px;
	}

	.ctl-label {
		font-weight: 600;
	}

	.ctl-val {
		font-size: var(--fs-sm);
		color: var(--text);
	}

	.ctl input[type='range'] {
		width: 100%;
		accent-color: var(--magenta);
	}

	.defmark {
		position: absolute;
		top: 38px;
		width: 2px;
		height: 14px;
		margin-left: -1px;
		background: #ffc94a;
		pointer-events: none;
	}

	.ctl input[type='checkbox'] {
		width: 20px;
		height: 20px;
		accent-color: var(--magenta);
	}

	.ctl .text {
		padding: 8px 12px;
		border-radius: var(--r-md);
		border: 1px solid var(--line-strong);
		background: rgba(0, 0, 0, 0.25);
		color: var(--text);
		font: inherit;
	}

	.choices {
		display: flex;
		flex-wrap: wrap;
		gap: 6px;
	}

	.choices button {
		border: 1px solid var(--line-strong);
		background: transparent;
		color: var(--text-dim);
		border-radius: 999px;
		padding: 5px 12px;
		font-size: var(--fs-sm);
		font-weight: 600;
		cursor: pointer;
	}

	.choices button.on {
		background: var(--text);
		color: var(--ink);
		border-color: var(--text);
	}

	.foot {
		align-items: flex-start;
	}

	.help {
		font-size: var(--fs-xs);
		color: var(--text-dim);
		line-height: 1.45;
	}

	.vibe-def {
		color: #ffc94a;
	}

	.toml-foot {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 10px;
	}

	@media (max-width: 960px) {
		.ft-grid {
			grid-template-columns: 1fr;
		}
		.ft-left {
			position: static;
		}
	}
</style>
