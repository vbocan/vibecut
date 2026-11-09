<script lang="ts">
	// Guided config editor — the same file the Raw TOML tab edits,
	// bound directly to `Config`'s own fields instead of a textarea.
	// Every field here maps 1:1 to a `vibecut::project_config::Config`
	// field via the generated types in `$lib/types/generated` — there
	// is exactly one schema, defined once in Rust; this component
	// never invents its own shape for anything it edits.
	//
	// Scope: every field of `Config` is reachable from some step except
	// `schema_version` (a constant, always 1 — no control needed) and
	// `texts` (`[[text]]`, top-level lower-thirds) — the latter is
	// deliberately NOT exposed here despite existing in the schema:
	// it's dead in the pipeline. `run_pipeline.rs` parses it, counts
	// it, and prints "(not yet rendered)" to the CLI before discarding
	// it — nothing ever reads a `Text` entry's `kind`/`content`/timing,
	// and no code path renders one. Building a control for it would be
	// a working-looking UI for a field that silently does nothing;
	// worse than not having the control at all. (Not to be confused
	// with `[intro]`/`[outro]`'s `TextCard.layers` below, a completely
	// separate, fully-wired mechanism this component does edit.)
	//
	// The fields a vibe's own template actually sets (basics, pacing,
	// transitions, soundtrack, intro/outro) get dedicated, purpose-
	// built controls. The long tail of rarely-touched fields (scoring
	// weights, LUT grading) lives in a single "Advanced" step with
	// plainer generic inputs — not every step deserves equal design
	// effort. Array-of-object fields (outputs, explicit input videos/
	// ranges/images, multi-file soundtracks, selection excludes/
	// weights, intro/outro layers, multi-angle groups and their per-
	// source offsets) get list editors: add/remove rows rather than a
	// single control, since there's no natural single-value widget for
	// "an arbitrary number of X".
	import { appState as s } from '$lib/state.svelte';
	import type { TextCard } from '$lib/types/generated/TextCard';
	import type { CardLayer } from '$lib/types/generated/CardLayer';
	import type { AngleGroupCfg } from '$lib/types/generated/AngleGroupCfg';
	import type { Soundtrack } from '$lib/types/generated/Soundtrack';
	import { radiogroup } from '$lib/radiogroup';

	const STEPS = [
		{ label: 'Basics' },
		{ label: 'Sources' },
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

	/** Shared by every optional-string field below (image timestamp,
	 *  output encoder preset and audio bitrate): empty input means "unset", not "set to empty
	 *  string" — the two are different values to a Rust `Option<T>`. */
	function optionalString(value: string): string | null {
		return value.trim() === '' ? null : value;
	}

	// ---------- input: explicit videos, manual ranges, images ----------
	function addVideo() {
		s.configStructured?.input.videos.push({ path: '' });
	}
	function removeVideo(i: number) {
		s.configStructured?.input.videos.splice(i, 1);
	}
	function addRange() {
		s.configStructured?.input.ranges.push({ file: '', start: '0s', end: '1s' });
	}
	function removeRange(i: number) {
		s.configStructured?.input.ranges.splice(i, 1);
	}
	function addImage() {
		s.configStructured?.input.images.push({
			path: '',
			duration: '4s',
			effect: 'auto',
			timestamp: null
		});
	}
	function removeImage(i: number) {
		s.configStructured?.input.images.splice(i, 1);
	}

	// ---------- outputs ----------
	//
	// The body renders once at the highest resolution among all
	// outputs, then transcodes to each output's own resolution/encoder
	// — so a second output is typically "same cut, different delivery
	// size" (e.g. a 4K master plus a 1080p upload copy), not a
	// different edit. New rows clone the last output's settings for
	// that reason, with a distinguishing path so the two don't collide
	// on disk.
	function addOutput() {
		const outs = s.configStructured?.outputs;
		if (!outs) return;
		const last = outs[outs.length - 1];
		const path = last ? last.path.replace(/(\.[a-zA-Z0-9]+)?$/, `_${outs.length + 1}$1`) : `out/output_${outs.length + 1}.mp4`;
		// A clone copies every key as it is, unset ones included: a platform
		// row leaves resolution / crf / audio unset so its preset decides
		// them, and an explicit default there would beat the preset.
		outs.push(
			last
				? { ...$state.snapshot(last), path }
				: { path, resolution: '1920x1080', encoder: 'auto', crf: 20, preset: null, audio_bitrate: '192k' }
		);
	}

	type OutputRow = NonNullable<typeof s.configStructured>['outputs'][number];
	// Encoder preset names `preset` held before it named platforms; the
	// engine still reads one there as the encoder preset
	// (output_format::is_encoder_preset_name).
	const LEGACY_ENCODER_PRESETS = new Set([
		'ultrafast', 'superfast', 'veryfast', 'faster', 'fast', 'medium', 'slow', 'slower', 'veryslow', 'placebo',
		'p1', 'p2', 'p3', 'p4', 'p5', 'p6', 'p7', 'default', 'hp', 'hq', 'bd', 'll', 'llhq', 'llhp', 'lossless',
		'losslesshp', 'speed', 'balanced', 'quality'
	]);
	/** The encoder preset an older config keeps in `preset`, if any. */
	function legacyEncoderPreset(out: OutputRow): string | null {
		const p = out.preset?.trim().toLowerCase();
		return p && LEGACY_ENCODER_PRESETS.has(p) ? p : null;
	}
	/** The Platform option a row's `preset` reads as: the engine treats
	 *  tiktok / shorts as reels, `master` (and a legacy encoder preset) as
	 *  the master. */
	function platformValue(out: OutputRow): string {
		const p = out.preset?.trim().toLowerCase() ?? '';
		if (p === 'tiktok' || p === 'shorts') return 'reels';
		if (p === 'master' || legacyEncoderPreset(out)) return '';
		return p;
	}
	/** Edit the encoder preset; a legacy one in `preset` moves out of the way. */
	function setEncoderPreset(out: OutputRow, value: string) {
		if (legacyEncoderPreset(out)) out.preset = null;
		out.encoder_preset = optionalString(value) ?? undefined;
	}
	/** Pick a platform preset (`''` = the master). A platform decides its
	 *  own size, quality and audio unless a key is set explicitly, so
	 *  choosing one clears those keys; back on the master, an output needs
	 *  a resolution again. A legacy encoder preset in `preset` moves to
	 *  `encoder_preset` first, so it isn't lost. */
	function setPlatform(out: OutputRow, value: string) {
		const legacy = legacyEncoderPreset(out);
		if (legacy && out.encoder_preset === undefined) out.encoder_preset = legacy;
		out.preset = value || null;
		if (out.preset) {
			out.resolution = null;
			out.crf = null;
			out.audio_bitrate = null;
		} else if (!out.resolution) {
			out.resolution = '1920x1080';
		}
	}
	function removeOutput(i: number) {
		const outs = s.configStructured?.outputs;
		// Config::validate() rejects an empty outputs list — the UI
		// just never offers the button that would get you there (see
		// the {#if cfg.outputs.length > 1} guard around the remove
		// button below) rather than surfacing a save-time error for it.
		if (!outs || outs.length <= 1) return;
		outs.splice(i, 1);
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
	/** Multiple files concatenate in declaration order into one virtual
	 *  track before beat-detection and looping — order matters, so
	 *  this is add-at-end / remove-by-index, not an unordered set. */
	function addSoundtrackFile(st: Soundtrack) {
		st.files.push('');
	}
	function removeSoundtrackFile(st: Soundtrack, i: number) {
		st.files.splice(i, 1);
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

	/** Layers draw in array order — later entries paint on top of
	 *  earlier ones — so add-at-end is the correct default position,
	 *  not just the simplest one. */
	function addTextLayer(card: TextCard) {
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
	function addImageLayer(card: TextCard) {
		card.layers.push({
			kind: 'image',
			path: '',
			position: 'center',
			height: 200,
			x_offset: 0,
			y_offset: 0,
			opacity: 1.0,
			fade_in: null,
			fade_out: null,
			appear_at: null,
			disappear_at: null
		});
	}
	function removeLayer(card: TextCard, index: number) {
		card.layers.splice(index, 1);
	}
	/** `appear_at`/`disappear_at`/per-layer fades are optional Durations
	 *  (`string | null`) — empty text input means "inherit the card's
	 *  own timing", which the pipeline already treats `null` as. */
	function optionalDuration(value: string): string | null {
		return value.trim() === '' ? null : value;
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
		<div class="step-tabs" role="tablist" aria-label="Settings steps" use:radiogroup>
			{#each STEPS as st, i (st.label)}
				<button
					type="button"
					class="step-tab"
					role="tab"
					id="wizard-tab-{i}"
					aria-controls="wizard-panel"
					aria-selected={step === i}
					class:active={step === i}
					onclick={() => (step = i)}
				>
					<span class="step-num" aria-hidden="true">{i + 1}</span>
					{st.label}
				</button>
			{/each}
		</div>

		<div class="step-body" role="tabpanel" id="wizard-panel" aria-labelledby="wizard-tab-{step}">
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

				<h3>Outputs</h3>
				<p class="dim">
					The body renders once at the largest master-shaped size among these, then transcodes to each one's own
					resolution/encoder — a second output is typically the same cut at a different delivery size, or a
					platform version (Reels, Square, WhatsApp) reframed with a blurred fill, a crop or black bars.
				</p>
				{#each cfg.outputs as out, i (i)}
					<div class="output-card">
						<div class="row wrap">
							<input class="text-input mono" placeholder="e.g. out/final.mp4" bind:value={out.path} />
							{#if cfg.outputs.length > 1}
								<button type="button" class="icon-btn" onclick={() => removeOutput(i)} aria-label="Remove output"
									>✕</button
								>
							{/if}
						</div>
						<div class="field-grid">
							<label class="field narrow">
								<span>Platform</span>
								<select
									class="text-input"
									value={platformValue(out)}
									onchange={(e) => setPlatform(out, (e.currentTarget as HTMLSelectElement).value)}
								>
									<option value="">Master</option>
									<option value="youtube">YouTube 16:9</option>
									<option value="reels">Reels / TikTok / Shorts 9:16</option>
									<option value="square">Square 1:1</option>
									<option value="whatsapp">WhatsApp 9:16 small</option>
								</select>
							</label>
							<label class="field narrow">
								<span>Fit</span>
								<select class="text-input" bind:value={out.fit}>
									<option value={undefined}>Blur fill</option>
									<option value="crop">Crop</option>
									<option value="pad">Black bars</option>
								</select>
							</label>
							<label class="field narrow">
								<span>Resolution</span>
								<select class="text-input" bind:value={out.resolution}>
									<option value={null}>From platform</option>
									<option value="1920x1080">1920×1080</option>
									<option value="3840x2160">3840×2160 (4K)</option>
									<option value="1280x720">1280×720</option>
									<option value="1080x1920">1080×1920 (vertical)</option>
									<option value="1080x1080">1080×1080 (square)</option>
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
								<input
									class="text-input mono"
									type="number"
									min="0"
									max="51"
									placeholder="auto"
									bind:value={out.crf}
								/>
							</label>
							<label class="field narrow">
								<span>Audio bitrate</span>
								<input
									class="text-input mono"
									placeholder="auto"
									value={out.audio_bitrate ?? ''}
									oninput={(e) => (out.audio_bitrate = optionalString((e.target as HTMLInputElement).value))}
								/>
							</label>
							<label class="field narrow">
								<span>Encoder preset</span>
								<input
									class="text-input mono"
									placeholder="auto"
									value={out.encoder_preset ?? legacyEncoderPreset(out) ?? ''}
									oninput={(e) => setEncoderPreset(out, (e.target as HTMLInputElement).value)}
								/>
							</label>
						</div>
					</div>
				{/each}
				<button type="button" class="chip" onclick={addOutput}>+ Add output</button>
			{:else if step === 1}
				<h3>Explicit video files</h3>
				<p class="dim">
					Extra files to include as sources alongside (or instead of) the discovered footage folder — each is
					probed and scored the same way.
				</p>
				{#each cfg.input.videos as v, i (i)}
					<div class="list-row">
						<input class="text-input mono" placeholder="/footage/extra_clip.mp4" bind:value={v.path} />
						<button type="button" class="icon-btn" onclick={() => removeVideo(i)} aria-label="Remove">✕</button>
					</div>
				{/each}
				<button type="button" class="chip" onclick={addVideo}>+ Add video file</button>

				<h3>Manual time ranges</h3>
				<p class="dim">
					Override automatic selection with an exact in/out range for a specific file — the picker uses this
					instead of scoring that file itself.
				</p>
				{#each cfg.input.ranges as r, i (i)}
					<div class="list-row">
						<input class="text-input mono" placeholder="filename, e.g. GH010320.MP4" bind:value={r.file} />
						<input class="text-input mono narrow-num" placeholder="start, e.g. 10s" bind:value={r.start} />
						<input class="text-input mono narrow-num" placeholder="end, e.g. 25s" bind:value={r.end} />
						<button type="button" class="icon-btn" onclick={() => removeRange(i)} aria-label="Remove">✕</button>
					</div>
				{/each}
				<button type="button" class="chip" onclick={addRange}>+ Add time range</button>

				<h3>Still images</h3>
				<p class="dim">
					Photos animated into clips and mixed into the output — treated as mandatory segments, always included
					alongside the auto-selected video.
				</p>
				{#each cfg.input.images as img, i (i)}
					<div class="image-input-card">
						<div class="row wrap">
							<input class="text-input mono" placeholder="/footage/photo.jpg" bind:value={img.path} />
							<button type="button" class="icon-btn" onclick={() => removeImage(i)} aria-label="Remove">✕</button>
						</div>
						<div class="field-grid">
							<label class="field narrow">
								<span>Hold duration</span>
								<input class="text-input mono" placeholder="e.g. 4s" bind:value={img.duration} />
							</label>
							<label class="field narrow">
								<span>Effect</span>
								<select class="text-input" bind:value={img.effect}>
									<option value="auto">Auto (from seed)</option>
									<option value="zoom_in">Zoom in</option>
									<option value="zoom_out">Zoom out</option>
									<option value="pan_left">Pan left</option>
									<option value="pan_right">Pan right</option>
									<option value="diagonal">Diagonal</option>
									<option value="rotate_zoom">Rotate + zoom</option>
									<option value="static">Static</option>
								</select>
							</label>
							<label class="field narrow">
								<span>Recorded at</span>
								<input
									class="text-input mono"
									placeholder="only used by strict_chronological"
									value={img.timestamp ?? ''}
									oninput={(e) => (img.timestamp = optionalString((e.target as HTMLInputElement).value))}
								/>
							</label>
						</div>
					</div>
				{/each}
				<button type="button" class="chip" onclick={addImage}>+ Add image</button>
			{:else if step === 2}
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
			{:else if step === 3}
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
			{:else if step === 4}
				{#if profile?.soundtrackNote}
					<p class="vibe-note">{profile.soundtrackNote}</p>
				{/if}
				{#if cfg.soundtrack}
					{@const st = cfg.soundtrack}
					<div class="row wrap">
						<span class="badge good">Soundtrack on</span>
						<button type="button" class="chip" onclick={removeSoundtrack}>Remove soundtrack</button>
					</div>
					<h4>Audio file{st.files.length > 1 ? 's' : ''} (server path)</h4>
					{#if st.files.length > 1}
						<p class="hint">Concatenated in this order into one virtual track before beat-detection.</p>
					{/if}
					{#each st.files as _, i (i)}
						<div class="list-row">
							<input class="text-input mono" placeholder="/projects/music/track.mp3" bind:value={st.files[i]} />
							<button
								type="button"
								class="icon-btn"
								onclick={() => removeSoundtrackFile(st, i)}
								aria-label="Remove">✕</button
							>
						</div>
					{/each}
					<button type="button" class="chip" onclick={() => addSoundtrackFile(st)}>+ Add file</button>
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
			{:else if step === 5}
				{#snippet cardEditor(card: TextCard)}
					<div class="field-grid">
						<label class="field narrow">
							<span>Duration</span>
							<input class="text-input mono" bind:value={card.duration} />
						</label>
						<label class="field narrow">
							<span>Mode</span>
							<select class="text-input" bind:value={card.mode}>
								<option value="card">Card</option>
								<option value="overlay">Overlay on footage</option>
							</select>
						</label>
						<label class="field narrow color-field">
							<span>Background</span>
							<span class="swatch" style="background:{card.bg_color}"></span>
							<input class="text-input mono" bind:value={card.bg_color} />
						</label>
					</div>

					<h4>Layers <span class="dim mono">({card.layers.length})</span></h4>
					<p class="hint">Drawn in order below — later layers paint on top of earlier ones.</p>
					{#each card.layers as layer, li (li)}
						<div class="layer-card">
							<div class="row wrap">
								<span class="badge">{layer.kind}</span>
								<button
									type="button"
									class="icon-btn"
									onclick={() => removeLayer(card, li)}
									aria-label="Remove layer">✕</button
								>
							</div>
							{#if layer.kind === 'text'}
								<label class="field">
									<span>Text</span>
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
									<label class="field narrow">
										<span>Position</span>
										<select class="text-input" bind:value={layer.position}>
											<option value="center">Center</option>
											<option value="top-left">Top left</option>
											<option value="top-center">Top center</option>
											<option value="top-right">Top right</option>
											<option value="bottom-left">Bottom left</option>
											<option value="bottom-center">Bottom center</option>
											<option value="bottom-right">Bottom right</option>
										</select>
									</label>
								</div>
							{:else}
								<label class="field">
									<span>Image path (server path)</span>
									<input class="text-input mono" placeholder="/projects/logos/brand.png" bind:value={layer.path} />
								</label>
								<div class="field-grid">
									<label class="field narrow">
										<span>Height (px)</span>
										<input class="text-input mono" type="number" min="1" bind:value={layer.height} />
									</label>
									<label class="field narrow">
										<span>Opacity</span>
										<input class="text-input mono" type="number" min="0" max="1" step="0.05" bind:value={layer.opacity} />
									</label>
									<label class="field narrow">
										<span>Position</span>
										<select class="text-input" bind:value={layer.position}>
											<option value="center">Center</option>
											<option value="top-left">Top left</option>
											<option value="top-center">Top center</option>
											<option value="top-right">Top right</option>
											<option value="bottom-left">Bottom left</option>
											<option value="bottom-center">Bottom center</option>
											<option value="bottom-right">Bottom right</option>
										</select>
									</label>
								</div>
							{/if}
							<div class="field-grid">
								<label class="field narrow">
									<span>X offset (px)</span>
									<input class="text-input mono" type="number" bind:value={layer.x_offset} />
								</label>
								<label class="field narrow">
									<span>Y offset (px)</span>
									<input class="text-input mono" type="number" bind:value={layer.y_offset} />
								</label>
								<label class="field narrow">
									<span>Appears at</span>
									<input
										class="text-input mono"
										placeholder="card start"
										value={layer.appear_at ?? ''}
										oninput={(e) => (layer.appear_at = optionalDuration((e.target as HTMLInputElement).value))}
									/>
								</label>
								<label class="field narrow">
									<span>Disappears at</span>
									<input
										class="text-input mono"
										placeholder="card end"
										value={layer.disappear_at ?? ''}
										oninput={(e) => (layer.disappear_at = optionalDuration((e.target as HTMLInputElement).value))}
									/>
								</label>
							</div>
						</div>
					{/each}
					<div class="row wrap">
						<button type="button" class="chip" onclick={() => addTextLayer(card)}>+ Add text layer</button>
						<button type="button" class="chip" onclick={() => addImageLayer(card)}>+ Add image layer</button>
					</div>
				{/snippet}

				<h3>Intro</h3>
				{#if cfg.intro}
					<button type="button" class="chip" onclick={removeIntro}>Remove intro</button>
					{@render cardEditor(cfg.intro)}
				{:else}
					<button type="button" class="btn primary" onclick={addIntro}>Add intro card</button>
				{/if}

				<h3>Outro</h3>
				{#if cfg.outro}
					<button type="button" class="chip" onclick={removeOutro}>Remove outro</button>
					{@render cardEditor(cfg.outro)}
				{:else}
					<button type="button" class="btn primary" onclick={addOutro}>Add outro card</button>
				{/if}
			{:else if step === 6}
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
			{:else if step === 7}
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
						<span>Gyro (spins, turns)</span>
						<input class="text-input mono" type="number" step="0.05" min="0" bind:value={cfg.scoring.gyro} />
					</label>
					<label class="field narrow">
						<span>Jumps (airtime)</span>
						<input class="text-input mono" type="number" step="0.05" min="0" bind:value={cfg.scoring.landings} />
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

	h4 {
		font-size: 11.5px;
		font-weight: 700;
		color: var(--text-dim);
		margin: 4px 0 -6px;
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

	.angle-group,
	.output-card,
	.layer-card,
	.image-input-card {
		display: flex;
		flex-direction: column;
		gap: 8px;
		background: var(--panel-alt);
		border: 1px solid var(--line);
		border-radius: 8px;
		padding: 12px;
	}

	.layer-card {
		border-left: 2px solid var(--accent-dim);
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
		background: var(--panel);
		color: var(--text-dim);
		border: 1px solid var(--line);
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
