<script lang="ts">
	// Guided config editor — the same file the Raw TOML tab edits,
	// bound directly to `Config`'s own fields instead of a textarea.
	// Every field here maps 1:1 to a `vibecut::project_config::Config`
	// field via the generated types in `$lib/types/generated` — there
	// is exactly one schema, defined once in Rust; this component
	// never invents its own shape for anything it edits.
	//
	// Scope: the fields a vibe's own template actually sets (basics,
	// pacing, transitions, soundtrack, one intro/outro text layer) get
	// dedicated, purpose-built controls, organized as wizard steps.
	// The long tail of rarely-touched fields (scoring weights, LUT
	// grading) lives in a single "Advanced" step with plainer generic
	// inputs — every `Config` field is reachable from *some* step, but
	// not every step deserves equal design effort. Array-of-object
	// fields (selection excludes/weights, multi-angle groups and their
	// per-source offsets) get list editors: add/remove rows rather
	// than a single control, since there's no natural single-value
	// widget for "an arbitrary number of X".
	import { appState as s } from '$lib/state.svelte';
	import type { TextCard } from '$lib/types/generated/TextCard';
	import type { CardLayer } from '$lib/types/generated/CardLayer';
	import type { AngleGroupCfg } from '$lib/types/generated/AngleGroupCfg';

	type TextCardLayer = Extract<CardLayer, { kind: 'text' }>;

	const STEPS = [
		{ label: 'Basics' },
		{ label: 'Selection & pacing' },
		{ label: 'Transitions' },
		{ label: 'Soundtrack' },
		{ label: 'Intro / outro' },
		{ label: 'Multi-camera' },
		{ label: 'Advanced' }
	];
	let step = $state(0);

	// ---------- per-vibe guidance ----------
	//
	// The schema is identical for every vibe — same Config, same
	// fields, same steps. What differs is which fields actually matter
	// for a given aesthetic, straight from each vibe's own rationale in
	// vibecut-ops/src/vibes.rs (kid_reel/action_highlight ramp and
	// beat-burst; family_chronicle/documentary duck for voices;
	// drone_cinematic has no source audio to duck; memory_keeper leans
	// on the soundtrack with few ducks). These notes surface that
	// context in place rather than making every vibe's wizard look
	// identical regardless of what was actually proposed.
	interface VibeProfile {
		transitionNote?: string;
		soundtrackNote?: string;
		emphasizeVad?: boolean;
	}
	const VIBE_PROFILES: Record<string, VibeProfile> = {
		kid_reel: {
			soundtrackNote:
				'This vibe leans on the soundtrack for pacing — beat-burst and speed-ramps read best with music underneath.'
		},
		family_chronicle: {
			transitionNote: 'Family chronicle keeps a steady pace by default — speed-ramps are off here.',
			soundtrackNote:
				'Ducking is tuned to let voices and laughter through. Turn up "speech duck weight" below if dialogue keeps getting buried.',
			emphasizeVad: true
		},
		drone_cinematic: {
			transitionNote: 'Drone cinematic holds its shots and never ramps — the calm pace is the point.',
			soundtrackNote: 'DJI footage usually has no source audio, so there is nothing to duck for — 0 duck windows is typical here.'
		},
		action_highlight: {
			soundtrackNote:
				'Soundtrack strongly recommended for this vibe — beat-burst and speed-ramps read best with a driving track underneath.'
		},
		documentary: {
			transitionNote: 'Documentary stays plain — no speed-ramps or kinetic effects, so the story stays the focus.',
			soundtrackNote:
				'Ducks hard on every detected speech moment so narration comes through clearly — "speech duck weight" is the key knob for this vibe.',
			emphasizeVad: true
		},
		memory_keeper: {
			transitionNote: 'Memory keeper holds a gentle, steady pace — speed-ramps are off by default.',
			soundtrackNote:
				'Soundtrack-led with few duck windows, so the music carries the mood instead of ducking around dialogue.'
		}
	};
	const profile = $derived(s.proposeResult ? VIBE_PROFILES[s.proposeResult.vibe] : undefined);

	// ---------- project.seed ----------
	//
	// `Project.seed` is a Rust `u64`, which ts-rs types as `bigint` —
	// but JSON has no bigint: `JSON.parse` hands back a plain
	// `number` on read, and `JSON.stringify` throws on an actual
	// `bigint` on write. The value is a number at every runtime
	// boundary that matters here; these two helpers are the only
	// place that pretends otherwise, to satisfy the generated type.
	function seedAsNumber(cfg: NonNullable<typeof s.configStructured>): number | null {
		return cfg.project.seed === null ? null : (cfg.project.seed as unknown as number);
	}
	function setSeed(cfg: NonNullable<typeof s.configStructured>, value: number | null) {
		cfg.project.seed = value === null ? null : (value as unknown as bigint);
	}

	// ---------- soundtrack ----------
	function addSoundtrack() {
		if (!s.configStructured) return;
		s.configStructured.soundtrack = {
			files: [],
			beat_align: true,
			energy_drives_pacing: true,
			ducking_db: -18,
			duck_count: 2,
			duck_min_spacing: '4s',
			duck_ramp: '300ms',
			duck_min_dur: '4s',
			duck_max_dur: '7s',
			fade_out: '1500ms',
			beat_burst: 'auto',
			beat_burst_min_beats: 8,
			beat_burst_max_beats: 12,
			vad_speech_weight: 1.5
		};
	}
	function removeSoundtrack() {
		if (!s.configStructured) return;
		s.configStructured.soundtrack = null;
	}
	function soundtrackFile(): string {
		return s.configStructured?.soundtrack?.files[0] ?? '';
	}
	function setSoundtrackFile(value: string) {
		if (!s.configStructured?.soundtrack) return;
		s.configStructured.soundtrack.files = value ? [value] : [];
	}

	// ---------- intro / outro cards ----------
	function newCard(): TextCard {
		return { duration: '3s', fade_in: '400ms', fade_out: '400ms', mode: 'card', bg_color: '#000000', layers: [] };
	}
	function addIntro() {
		if (s.configStructured) s.configStructured.intro = newCard();
	}
	function removeIntro() {
		if (s.configStructured) s.configStructured.intro = null;
	}
	function addOutro() {
		if (s.configStructured) s.configStructured.outro = newCard();
	}
	function removeOutro() {
		if (s.configStructured) s.configStructured.outro = null;
	}

	/** The card's first text layer, if it has one — every vibe
	 *  template's intro/outro is exactly this shape (one title line),
	 *  so editing "the" text layer covers the common case without a
	 *  full layer-list editor. */
	function titleLayer(card: TextCard): TextCardLayer | null {
		const l = card.layers.find((l): l is TextCardLayer => l.kind === 'text');
		return l ?? null;
	}
	function addTitleLayer(card: TextCard) {
		card.layers.push({
			kind: 'text',
			content: '',
			font: null,
			size: 96,
			color: '#FFFFFF',
			position: 'center',
			y_offset: 0,
			x_offset: 0,
			fade_in: null,
			fade_out: null,
			appear_at: null,
			disappear_at: null,
			background: null
		});
	}

	// ---------- selection: source weights & excludes ----------
	function addSourceWeight() {
		s.configStructured?.selection.source_weights.push({ source_id: '', weight: 1.0 });
	}
	function removeSourceWeight(i: number) {
		s.configStructured?.selection.source_weights.splice(i, 1);
	}
	function addExclude() {
		s.configStructured?.selection.exclude.push({ file: '', start: '0s', end: '1s' });
	}
	function removeExclude(i: number) {
		s.configStructured?.selection.exclude.splice(i, 1);
	}

	// ---------- multi-angle groups ----------
	function addAngleGroup() {
		s.configStructured?.multi_angle.groups.push({ master: '', members: [], offsets: {} });
	}
	function removeAngleGroup(i: number) {
		s.configStructured?.multi_angle.groups.splice(i, 1);
	}
	function membersText(grp: AngleGroupCfg): string {
		return grp.members.join(', ');
	}
	function setMembersText(grp: AngleGroupCfg, text: string) {
		grp.members = text
			.split(',')
			.map((m) => m.trim())
			.filter(Boolean);
	}
	/** Manual offsets are a `source_id -> duration` map, only consulted
	 *  when `sync = "manual"`. A new row gets a random placeholder key
	 *  (object keys must be unique and non-empty) that the user renames
	 *  to a real source id via the same input.
	 *
	 *  Every mutator below takes the row's *position* in
	 *  `Object.entries(grp.offsets)`, not the key string itself.
	 *  Renaming a key can't happen in place — it's delete-old +
	 *  insert-new, which reassigns `grp.offsets` to a fresh object —
	 *  so a handler closure that captured the OLD key string goes
	 *  stale the instant a rename commits. Re-deriving "whichever key
	 *  is currently at index j" at call time sidesteps that: the row's
	 *  position is what's actually stable across a rename, not its key. */
	function addOffset(grp: AngleGroupCfg) {
		grp.offsets[crypto.randomUUID()] = '0s';
	}
	function renameOffsetKeyAt(grp: AngleGroupCfg, index: number, newKey: string) {
		const entries = Object.entries(grp.offsets);
		if (index >= entries.length || !newKey) return;
		const [oldKey] = entries[index];
		if (oldKey === newKey) return;
		const rebuilt: Record<string, string> = {};
		entries.forEach(([k, v], i) => {
			rebuilt[i === index ? newKey : k] = v;
		});
		grp.offsets = rebuilt;
	}
	function setOffsetValueAt(grp: AngleGroupCfg, index: number, value: string) {
		const entries = Object.entries(grp.offsets);
		if (index >= entries.length) return;
		grp.offsets[entries[index][0]] = value;
	}
	function removeOffsetAt(grp: AngleGroupCfg, index: number) {
		const entries = Object.entries(grp.offsets);
		if (index >= entries.length) return;
		delete grp.offsets[entries[index][0]];
	}
</script>

{#if s.configStructured}
	{@const cfg = s.configStructured}
	<div class="wizard">
		<div class="step-tabs">
			{#each STEPS as st, i (st.label)}
				<button type="button" class="step-tab" class:active={step === i} onclick={() => (step = i)}>
					<span class="step-num">{i + 1}</span>
					{st.label}
				</button>
			{/each}
		</div>

		<div class="step-body">
			{#if step === 0}
				<div class="field-grid">
					<label class="field">
						<span>Title</span>
						<input class="text-input" bind:value={cfg.project.name} />
					</label>
					<label class="field narrow">
						<span>Target duration</span>
						<input class="text-input mono" placeholder="e.g. 90s, 3m" bind:value={cfg.project.target_duration} />
					</label>
					<label class="field narrow">
						<span>FPS</span>
						<input class="text-input mono" type="number" min="1" bind:value={cfg.project.fps} />
					</label>
					<label class="field narrow">
						<span>Seed</span>
						<input
							class="text-input mono"
							type="number"
							value={seedAsNumber(cfg)}
							oninput={(e) => setSeed(cfg, (e.target as HTMLInputElement).valueAsNumber || null)}
						/>
					</label>
					<label class="field narrow">
						<span>Fade in</span>
						<input class="text-input mono" placeholder="e.g. 300ms" bind:value={cfg.project.fade_in} />
					</label>
					<label class="field narrow">
						<span>Fade out</span>
						<input class="text-input mono" placeholder="e.g. 1500ms" bind:value={cfg.project.fade_out} />
					</label>
				</div>

				{#if cfg.outputs.length > 0}
					{@const out = cfg.outputs[0]}
					<h3>Output</h3>
					<div class="field-grid">
						<label class="field narrow">
							<span>Resolution</span>
							<select class="text-input" bind:value={out.resolution}>
								<option value="1920x1080">1920×1080</option>
								<option value="3840x2160">3840×2160 (4K)</option>
								<option value="1280x720">1280×720</option>
							</select>
						</label>
						<label class="field narrow">
							<span>Encoder</span>
							<select class="text-input" bind:value={out.encoder}>
								<option value="auto">auto</option>
								<option value="libx264">libx264</option>
								<option value="libx265">libx265</option>
								<option value="nvenc">nvenc</option>
								<option value="amf">amf</option>
								<option value="qsv">qsv</option>
							</select>
						</label>
						<label class="field narrow">
							<span>Quality (CRF)</span>
							<input class="text-input mono" type="number" min="0" max="51" bind:value={out.crf} />
						</label>
						<label class="field narrow">
							<span>Audio bitrate</span>
							<input class="text-input mono" placeholder="e.g. 192k" bind:value={out.audio_bitrate} />
						</label>
					</div>
					{#if cfg.outputs.length > 1}
						<p class="dim">
							{cfg.outputs.length - 1} more output{cfg.outputs.length > 2 ? 's' : ''} configured — edit via Raw TOML.
						</p>
					{/if}
				{/if}
			{:else if step === 1}
				<div class="field-grid">
					<label class="field narrow">
						<span>Min clip length</span>
						<input class="text-input mono" placeholder="e.g. 1.5s" bind:value={cfg.selection.min_clip} />
					</label>
					<label class="field narrow">
						<span>Max clip length</span>
						<input class="text-input mono" placeholder="e.g. 6s" bind:value={cfg.selection.max_clip} />
					</label>
					<label class="field narrow">
						<span>Order</span>
						<select class="text-input" bind:value={cfg.selection.order}>
							<option value="chronological">Chronological</option>
							<option value="energy_arc">Energy arc</option>
							<option value="strict_chronological">Strict chronological</option>
						</select>
					</label>
					<label class="field narrow">
						<span>Max clips per source</span>
						<input
							class="text-input mono"
							type="number"
							min="1"
							placeholder="auto"
							value={cfg.selection.max_per_source}
							oninput={(e) =>
								(cfg.selection.max_per_source = (e.target as HTMLInputElement).valueAsNumber || null)}
						/>
					</label>
				</div>
				<label class="field slider">
					<span>Variety <span class="mono dim">{cfg.selection.variety.toFixed(2)}</span></span>
					<input type="range" min="0" max="1" step="0.01" bind:value={cfg.selection.variety} />
					<span class="hint">0 = same seed always picks the same clips. Higher = more shake-up between renders.</span>
				</label>

				<h3>Source weights</h3>
				<p class="dim">
					Boost or reduce how often a source's clips get picked. 1.0 = no change, 0 = never picked — same effect as
					the MoreOf/LessOf feedback actions, editable directly.
				</p>
				{#each cfg.selection.source_weights as sw, i (i)}
					<div class="list-row">
						<input class="text-input mono" placeholder="source id, e.g. 0320" bind:value={sw.source_id} />
						<input
							class="text-input mono narrow-num"
							type="number"
							step="0.1"
							min="0"
							max="10"
							bind:value={sw.weight}
						/>
						<button type="button" class="icon-btn" onclick={() => removeSourceWeight(i)} aria-label="Remove">✕</button
						>
					</div>
				{/each}
				<button type="button" class="chip" onclick={addSourceWeight}>+ Add source weight</button>

				<h3>Excluded ranges</h3>
				<p class="dim">
					Windows the picker must avoid, by source filename (or filename suffix) and chapter-local time range —
					same effect as "drop the bit at M:SS" feedback, editable directly.
				</p>
				{#each cfg.selection.exclude as ex, i (i)}
					<div class="list-row">
						<input class="text-input mono" placeholder="filename, e.g. GH010320.MP4" bind:value={ex.file} />
						<input class="text-input mono narrow-num" placeholder="start, e.g. 30s" bind:value={ex.start} />
						<input class="text-input mono narrow-num" placeholder="end, e.g. 45s" bind:value={ex.end} />
						<button type="button" class="icon-btn" onclick={() => removeExclude(i)} aria-label="Remove">✕</button>
					</div>
				{/each}
				<button type="button" class="chip" onclick={addExclude}>+ Add exclude</button>
			{:else if step === 2}
				{#if profile?.transitionNote}
					<p class="vibe-note">{profile.transitionNote}</p>
				{/if}
				<div class="field-grid">
					<label class="field narrow">
						<span>Transition</span>
						<input class="text-input" placeholder="e.g. fade" bind:value={cfg.transition.default} />
					</label>
					<label class="field narrow">
						<span>Duration</span>
						<input class="text-input mono" placeholder="e.g. 400ms" bind:value={cfg.transition.duration} />
					</label>
				</div>
				<h3>Speed-ramp ("whip-up")</h3>
				<label class="field slider">
					<span
						>Probability <span class="mono dim">{(cfg.transition.speed_ramp_probability * 100).toFixed(0)}%</span
						></span
					>
					<input type="range" min="0" max="1" step="0.05" bind:value={cfg.transition.speed_ramp_probability} />
					<span class="hint">How many transitions get the ramp. 0 = never, 1 = every seam.</span>
				</label>
				<div class="field-grid">
					<label class="field narrow">
						<span>Ramp duration</span>
						<input class="text-input mono" placeholder="e.g. 2000ms" bind:value={cfg.transition.speed_ramp_duration} />
					</label>
					<label class="field narrow">
						<span>Ramp speed factor</span>
						<input
							class="text-input mono"
							type="number"
							min="1"
							max="8"
							step="0.5"
							bind:value={cfg.transition.speed_ramp_factor}
						/>
					</label>
					<label class="field checkbox">
						<input type="checkbox" bind:checked={cfg.transition.speed_ramp_flash} />
						<span>Quik-style flash on the ramp</span>
					</label>
				</div>
			{:else if step === 3}
				{#if profile?.soundtrackNote}
					<p class="vibe-note">{profile.soundtrackNote}</p>
				{/if}
				{#if cfg.soundtrack}
					{@const st = cfg.soundtrack}
					<div class="row wrap">
						<span class="badge good">Soundtrack on</span>
						<button type="button" class="chip" onclick={removeSoundtrack}>Remove soundtrack</button>
					</div>
					<label class="field">
						<span>Audio file (server path)</span>
						<input
							class="text-input mono"
							placeholder="/projects/music/track.mp3"
							value={soundtrackFile()}
							oninput={(e) => setSoundtrackFile((e.target as HTMLInputElement).value)}
						/>
					</label>
					<div class="field-grid">
						<label class="field checkbox">
							<input type="checkbox" bind:checked={st.beat_align} />
							<span>Beat-align cuts</span>
						</label>
						<label class="field narrow">
							<span>Beat-burst</span>
							<select class="text-input" bind:value={st.beat_burst}>
								<option value="auto">Auto</option>
								<option value="on">On</option>
								<option value="off">Off</option>
							</select>
						</label>
						<label class="field narrow">
							<span>Duck count</span>
							<input class="text-input mono" type="number" min="0" bind:value={st.duck_count} />
						</label>
						<label class="field narrow">
							<span>Ducking (dB)</span>
							<input class="text-input mono" type="number" max="0" bind:value={st.ducking_db} />
						</label>
					</div>
					{#if profile?.emphasizeVad}
						<label class="field slider">
							<span>Speech duck weight <span class="mono dim">{st.vad_speech_weight.toFixed(1)}</span></span>
							<input type="range" min="0" max="3" step="0.1" bind:value={st.vad_speech_weight} />
							<span class="hint">
								How strongly detected speech boosts a moment's duck priority. Higher = the soundtrack ducks more
								readily whenever someone's talking. Only has an effect if a voice-activity model is present on the
								server.
							</span>
						</label>
					{:else}
						<label class="field narrow">
							<span>Speech duck weight</span>
							<input class="text-input mono" type="number" min="0" step="0.1" bind:value={st.vad_speech_weight} />
						</label>
					{/if}
				{:else}
					<p class="dim">No soundtrack configured — the picker still produces a watchable cut, just with no music-driven pacing.</p>
					<button type="button" class="btn primary" onclick={addSoundtrack}>Add soundtrack</button>
				{/if}
			{:else if step === 4}
				<h3>Intro</h3>
				{#if cfg.intro}
					{@const layer = titleLayer(cfg.intro)}
					<div class="row wrap">
						<button type="button" class="chip" onclick={removeIntro}>Remove intro</button>
					</div>
					<div class="field-grid">
						<label class="field narrow">
							<span>Duration</span>
							<input class="text-input mono" bind:value={cfg.intro.duration} />
						</label>
						<label class="field narrow">
							<span>Mode</span>
							<select class="text-input" bind:value={cfg.intro.mode}>
								<option value="card">Card</option>
								<option value="overlay">Overlay on footage</option>
							</select>
						</label>
						<label class="field narrow color-field">
							<span>Background</span>
							<span class="swatch" style="background:{cfg.intro.bg_color}"></span>
							<input class="text-input mono" bind:value={cfg.intro.bg_color} />
						</label>
					</div>
					{#if layer}
						<label class="field">
							<span>Title text</span>
							<input class="text-input" bind:value={layer.content} />
						</label>
						<div class="field-grid">
							<label class="field narrow">
								<span>Size</span>
								<input class="text-input mono" type="number" min="8" bind:value={layer.size} />
							</label>
							<label class="field narrow color-field">
								<span>Color</span>
								<span class="swatch" style="background:{layer.color}"></span>
								<input class="text-input mono" bind:value={layer.color} />
							</label>
						</div>
					{:else}
						<button type="button" class="chip" onclick={() => addTitleLayer(cfg.intro!)}>Add title text</button>
					{/if}
				{:else}
					<button type="button" class="btn primary" onclick={addIntro}>Add intro card</button>
				{/if}

				<h3>Outro</h3>
				{#if cfg.outro}
					{@const layer = titleLayer(cfg.outro)}
					<div class="row wrap">
						<button type="button" class="chip" onclick={removeOutro}>Remove outro</button>
					</div>
					<div class="field-grid">
						<label class="field narrow">
							<span>Duration</span>
							<input class="text-input mono" bind:value={cfg.outro.duration} />
						</label>
						<label class="field narrow">
							<span>Mode</span>
							<select class="text-input" bind:value={cfg.outro.mode}>
								<option value="card">Card</option>
								<option value="overlay">Overlay on footage</option>
							</select>
						</label>
						<label class="field narrow color-field">
							<span>Background</span>
							<span class="swatch" style="background:{cfg.outro.bg_color}"></span>
							<input class="text-input mono" bind:value={cfg.outro.bg_color} />
						</label>
					</div>
					{#if layer}
						<label class="field">
							<span>Text</span>
							<input class="text-input" bind:value={layer.content} />
						</label>
					{:else}
						<button type="button" class="chip" onclick={() => addTitleLayer(cfg.outro!)}>Add text</button>
					{/if}
				{:else}
					<button type="button" class="btn primary" onclick={addOutro}>Add outro card</button>
				{/if}
			{:else if step === 5}
				<label class="field checkbox">
					<input type="checkbox" bind:checked={cfg.multi_angle.enabled} />
					<span>Enable multi-camera grouping</span>
				</label>
				{#if cfg.multi_angle.enabled}
					<div class="field-grid">
						<label class="field narrow">
							<span>Sync method</span>
							<select class="text-input" bind:value={cfg.multi_angle.sync}>
								<option value="auto">Auto</option>
								<option value="creation_time">Creation time</option>
								<option value="audio">Audio cross-correlate</option>
								<option value="manual">Manual offsets</option>
							</select>
						</label>
						<label class="field narrow">
							<span>Min hold</span>
							<input class="text-input mono" placeholder="e.g. 1.5s" bind:value={cfg.multi_angle.min_hold} />
						</label>
					</div>
					<p class="dim">
						Leave groups empty to auto-discover from overlapping recording windows, or define them explicitly
						below. Each group's master camera is implicit — list only the other members.
					</p>
					{#each cfg.multi_angle.groups as grp, i (i)}
						<div class="angle-group">
							<div class="row wrap">
								<input class="text-input mono narrow-num" placeholder="master source id" bind:value={grp.master} />
								<input
									class="text-input mono"
									placeholder="member ids, comma-separated"
									value={membersText(grp)}
									oninput={(e) => setMembersText(grp, (e.target as HTMLInputElement).value)}
								/>
								<button type="button" class="icon-btn" onclick={() => removeAngleGroup(i)} aria-label="Remove group"
									>✕</button
								>
							</div>
							{#if cfg.multi_angle.sync === 'manual'}
								<p class="hint">
									Manual offsets (source id → duration, sign optional): member_file_time = master_scene_time +
									offset.
								</p>
								{#each Object.entries(grp.offsets) as [key, value], j (j)}
									<div class="list-row">
										<input
											class="text-input mono"
											value={key}
											oninput={(e) => renameOffsetKeyAt(grp, j, (e.target as HTMLInputElement).value)}
										/>
										<input
											class="text-input mono narrow-num"
											placeholder="e.g. +2.4s"
											value={value}
											oninput={(e) => setOffsetValueAt(grp, j, (e.target as HTMLInputElement).value)}
										/>
										<button
											type="button"
											class="icon-btn"
											onclick={() => removeOffsetAt(grp, j)}
											aria-label="Remove offset">✕</button
										>
									</div>
								{/each}
								<button type="button" class="chip" onclick={() => addOffset(grp)}>+ Add offset</button>
							{/if}
						</div>
					{/each}
					<button type="button" class="chip" onclick={addAngleGroup}>+ Add angle group</button>
				{/if}
			{:else if step === 6}
				<h3>Scoring weights</h3>
				<p class="dim">How much each signal contributes to a moment's score. Higher = more influence on what gets picked.</p>
				<div class="field-grid">
					<label class="field narrow">
						<span>HiLight tags</span>
						<input class="text-input mono" type="number" step="0.05" min="0" bind:value={cfg.scoring.hilight_tags} />
					</label>
					<label class="field narrow">
						<span>Accelerometer peaks</span>
						<input class="text-input mono" type="number" step="0.05" min="0" bind:value={cfg.scoring.accel_peaks} />
					</label>
					<label class="field narrow">
						<span>GPS speed</span>
						<input class="text-input mono" type="number" step="0.05" min="0" bind:value={cfg.scoring.gps_speed} />
					</label>
					<label class="field narrow">
						<span>Faces</span>
						<input class="text-input mono" type="number" step="0.05" min="0" bind:value={cfg.scoring.faces} />
					</label>
					<label class="field narrow">
						<span>Motion peaks</span>
						<input class="text-input mono" type="number" step="0.05" min="0" bind:value={cfg.scoring.motion_peaks} />
					</label>
					<label class="field narrow">
						<span>Diversity per source</span>
						<input
							class="text-input mono"
							type="number"
							step="0.05"
							min="0"
							bind:value={cfg.scoring.diversity_per_source}
						/>
					</label>
					<label class="field narrow">
						<span>Sharpness floor</span>
						<input
							class="text-input mono"
							type="number"
							step="0.05"
							min="0"
							max="1"
							bind:value={cfg.scoring.sharpness_floor}
						/>
					</label>
					<label class="field narrow">
						<span>Exposure floor</span>
						<input
							class="text-input mono"
							type="number"
							step="0.05"
							min="0"
							max="1"
							bind:value={cfg.scoring.exposure_floor}
						/>
					</label>
				</div>

				<h3>Grading</h3>
				<label class="field checkbox">
					<input type="checkbox" bind:checked={cfg.grading.stabilize} />
					<span>Stabilize (only for non-HyperSmooth / shaky sources)</span>
				</label>
			{/if}
		</div>

		<div class="wizard-footer">
			<div class="row">
				<button type="button" class="btn" disabled={step === 0} onclick={() => step--}>Back</button>
				<button type="button" class="btn" disabled={step === STEPS.length - 1} onclick={() => step++}>Next</button>
			</div>
			<button
				class="btn primary"
				disabled={!s.structuredConfigDirty || s.savingConfig}
				onclick={() => s.saveStructuredConfig()}
			>
				{s.savingConfig ? 'Saving…' : s.structuredConfigDirty ? 'Save' : 'Saved'}
			</button>
		</div>
		{#if s.configStructuredError}
			<p class="error">{s.configStructuredError}</p>
		{/if}
	</div>
{:else if s.configStructuredError}
	<p class="error">{s.configStructuredError}</p>
{:else}
	<p class="dim">Loading…</p>
{/if}

<style>
	.wizard {
		display: flex;
		flex-direction: column;
		gap: 16px;
	}

	.step-tabs {
		display: flex;
		flex-wrap: wrap;
		gap: 6px;
		border-bottom: 1px solid var(--line);
		padding-bottom: 12px;
	}

	.step-tab {
		display: flex;
		align-items: center;
		gap: 6px;
		background: transparent;
		border: 1px solid transparent;
		color: var(--text-dim);
		padding: 6px 10px;
		border-radius: 6px;
		font-size: 12.5px;
		font-weight: 600;
		cursor: pointer;
	}

	.step-tab:hover {
		color: var(--text);
	}

	.step-tab.active {
		background: var(--panel-alt);
		border-color: var(--line);
		color: var(--accent);
	}

	.step-num {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		width: 16px;
		height: 16px;
		border-radius: 4px;
		background: var(--ink);
		font-family: var(--mono);
		font-size: 10px;
	}

	.step-body {
		display: flex;
		flex-direction: column;
		gap: 14px;
		min-height: 180px;
	}

	h3 {
		font-size: 12px;
		text-transform: uppercase;
		letter-spacing: 0.05em;
		color: var(--text-faint);
		margin: 6px 0 -4px;
	}

	.field-grid {
		display: flex;
		flex-wrap: wrap;
		gap: 12px;
	}

	.field {
		display: flex;
		flex-direction: column;
		gap: 5px;
		font-size: 12px;
		color: var(--text-dim);
		flex: 1 1 220px;
	}

	.field.narrow {
		flex: 0 0 160px;
	}

	.field.checkbox {
		flex-direction: row;
		align-items: center;
		gap: 8px;
		flex: 0 0 auto;
	}

	.field.slider {
		flex: 1 1 100%;
	}

	.field.slider input[type='range'] {
		accent-color: var(--accent);
	}

	.hint {
		color: var(--text-faint);
		font-size: 11px;
	}

	.vibe-note {
		background: var(--panel-alt);
		border-left: 2px solid var(--accent);
		border-radius: 0 6px 6px 0;
		padding: 8px 12px;
		font-size: 12.5px;
		color: var(--text-dim);
		margin: 0;
	}

	.color-field {
		flex-direction: row;
		align-items: center;
		flex-wrap: wrap;
	}

	.color-field span:first-child {
		flex: 0 0 100%;
	}

	.swatch {
		width: 18px;
		height: 18px;
		border-radius: 4px;
		border: 1px solid var(--line);
		flex-shrink: 0;
	}

	.text-input {
		background: var(--panel-alt);
		border: 1px solid var(--line);
		border-radius: 6px;
		padding: 7px 10px;
		font-size: 13px;
		color: var(--text);
		width: 100%;
	}

	.mono {
		font-family: var(--mono);
	}

	.dim {
		color: var(--text-faint);
		font-size: 12.5px;
	}

	.row {
		display: flex;
		align-items: center;
		gap: 10px;
	}

	.list-row {
		display: flex;
		align-items: center;
		gap: 8px;
	}

	.list-row .text-input {
		width: auto;
		flex: 1;
	}

	.narrow-num {
		width: 130px;
		flex: 0 0 130px;
	}

	.icon-btn {
		flex: 0 0 auto;
		width: 28px;
		height: 28px;
		display: flex;
		align-items: center;
		justify-content: center;
		background: var(--panel-alt);
		border: 1px solid var(--line);
		border-radius: 6px;
		color: var(--text-faint);
		font-size: 12px;
		cursor: pointer;
	}

	.icon-btn:hover {
		border-color: var(--bad);
		color: var(--bad);
	}

	.angle-group {
		display: flex;
		flex-direction: column;
		gap: 8px;
		background: var(--panel-alt);
		border: 1px solid var(--line);
		border-radius: 8px;
		padding: 12px;
	}

	.row.wrap {
		flex-wrap: wrap;
	}

	.btn {
		border: 1px solid var(--line);
		background: var(--panel-alt);
		color: var(--text);
		padding: 8px 16px;
		border-radius: 6px;
		font-size: 13px;
		font-weight: 600;
		cursor: pointer;
		white-space: nowrap;
	}

	.btn:disabled {
		opacity: 0.45;
		cursor: not-allowed;
	}

	.btn.primary {
		background: var(--accent);
		border-color: var(--accent);
		color: #1a0e06;
	}

	.btn.primary:hover:not(:disabled) {
		filter: brightness(1.08);
	}

	.chip {
		border: 1px solid var(--line);
		background: var(--panel-alt);
		color: var(--text);
		padding: 6px 12px;
		border-radius: 999px;
		font-size: 12.5px;
		font-weight: 600;
		cursor: pointer;
	}

	.chip:hover {
		border-color: var(--accent);
		color: var(--accent);
	}

	.badge {
		font-size: 10.5px;
		font-weight: 700;
		text-transform: uppercase;
		letter-spacing: 0.04em;
		padding: 2px 6px;
		border-radius: 4px;
	}

	.badge.good {
		background: rgba(92, 201, 138, 0.16);
		color: var(--good);
	}

	.wizard-footer {
		display: flex;
		align-items: center;
		justify-content: space-between;
		border-top: 1px solid var(--line);
		padding-top: 14px;
	}

	.error {
		color: var(--bad);
		font-size: 13px;
		margin: 0;
	}
</style>
