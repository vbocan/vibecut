// Intro / outro card helpers for the Titles editor: where the renderer
// puts a layer (so the preview matches), when a layer shows and how
// faded it is at a moment of the card, and sensible new cards and lines.
// Pure functions: `node --test src/lib/titles.test.mjs` runs their tests.

/** "7s", "700ms", "1s 500ms" → seconds; NaN when unreadable (as finetune.ts). */
function parseDuration(s: unknown): number {
	if (typeof s === 'number') return s;
	if (typeof s !== 'string') return NaN;
	let total = 0;
	let any = false;
	for (const m of s.matchAll(/(\d+(?:\.\d+)?)\s*(ms|s|m|h)/g)) {
		any = true;
		const n = Number(m[1]);
		total += m[2] === 'ms' ? n / 1000 : m[2] === 's' ? n : m[2] === 'm' ? n * 60 : n * 3600;
	}
	return any ? total : NaN;
}

/** The anchors the renderer knows (video_renderer::position_to_xy), as a 3×3 grid; null cells don't exist. */
export const ANCHOR_GRID: (string | null)[] = [
	'top-left',
	'top-center',
	'top-right',
	null,
	'center',
	null,
	'bottom-left',
	'bottom-center',
	'bottom-right'
];

export const ANCHOR_LABELS: Record<string, string> = {
	'top-left': 'Top left',
	'top-center': 'Top centre',
	'top-right': 'Top right',
	center: 'Centre',
	'bottom-left': 'Bottom left',
	'bottom-center': 'Bottom centre',
	'bottom-right': 'Bottom right'
};

/**
 * CSS placement of a layer in a frame, in frame pixels (scale them for the
 * preview). Mirrors the renderer: 20 px from the edges, 40 px from the
 * bottom for bottom-centre and bottom-right text; offsets move right/down.
 */
export function placement(
	position: string,
	xOff: number,
	yOff: number,
	kind: 'text' | 'image' = 'text'
): { left?: number; right?: number; top?: number; bottom?: number; centerX: boolean; centerY: boolean } {
	const x = xOff || 0;
	const y = yOff || 0;
	const deep = kind === 'text' ? 40 : 20;
	switch (position) {
		case 'top-left':
			return { left: 20 + x, top: 20 + y, centerX: false, centerY: false };
		case 'top-right':
			return { right: 20 - x, top: 20 + y, centerX: false, centerY: false };
		case 'top-center':
			return { left: x, top: 20 + y, centerX: true, centerY: false };
		case 'bottom-left':
			return { left: 20 + x, bottom: 20 - y, centerX: false, centerY: false };
		case 'bottom-right':
			return { right: 20 - x, bottom: deep - y, centerX: false, centerY: false };
		case 'bottom-center':
			return { left: x, bottom: deep - y, centerX: true, centerY: false };
		default:
			return { left: x, top: y, centerX: true, centerY: true };
	}
}

/** `placement` as an inline style at `scale` (preview px per frame px). */
export function placementStyle(p: ReturnType<typeof placement>, scale: number): string {
	const px = (v: number) => `${(v * scale).toFixed(2)}px`;
	const parts: string[] = [];
	if (p.centerX) parts.push(`left: calc(50% + ${px(p.left ?? 0)})`);
	else if (p.left !== undefined) parts.push(`left: ${px(p.left)}`);
	else if (p.right !== undefined) parts.push(`right: ${px(p.right)}`);
	if (p.centerY) parts.push(`top: calc(50% + ${px(p.top ?? 0)})`);
	else if (p.top !== undefined) parts.push(`top: ${px(p.top)}`);
	else if (p.bottom !== undefined) parts.push(`bottom: ${px(p.bottom)}`);
	const tx = p.centerX ? '-50%' : '0';
	const ty = p.centerY ? '-50%' : '0';
	parts.push(`transform: translate(${tx}, ${ty})`);
	return parts.join('; ');
}

export interface Timed {
	appear_at?: string | null;
	disappear_at?: string | null;
	fade_in?: string | null;
	fade_out?: string | null;
}

const secs = (v: string | null | undefined, dflt: number) => {
	const n = parseDuration(v ?? '');
	return Number.isFinite(n) ? n : dflt;
};

/** How visible a layer is at `t` seconds into the card (0..1). */
export function layerOpacity(layer: Timed, card: { duration: string; fade_in: string; fade_out: string }, t: number): number {
	const dur = secs(card.duration, 4);
	const start = secs(layer.appear_at, 0);
	const end = Math.min(secs(layer.disappear_at, dur), dur);
	if (t < start || t > end) return 0;
	const fi = secs(layer.fade_in, secs(card.fade_in, 0));
	const fo = secs(layer.fade_out, secs(card.fade_out, 0));
	const a = fi > 0 ? Math.min(1, (t - start) / fi) : 1;
	const b = fo > 0 ? Math.min(1, (end - t) / fo) : 1;
	return Math.max(0, Math.min(a, b));
}

/** Fonts the image ships (DOCKER.md, "Fonts and models"), as a config names them. */
export const FONTS: { value: string | null; label: string; css: string }[] = [
	{ value: null, label: 'Default (Inter)', css: 'Inter, system-ui, sans-serif' },
	{ value: 'Inter', label: 'Inter', css: 'Inter, system-ui, sans-serif' },
	{ value: 'Inter Display:bold', label: 'Inter Display, bold', css: '"Inter Display", Inter, system-ui, sans-serif' },
	{ value: 'Bebas Neue', label: 'Bebas Neue', css: '"Bebas Neue", Impact, "Arial Narrow", sans-serif' },
	{ value: 'DejaVu Sans', label: 'DejaVu Sans', css: '"DejaVu Sans", Verdana, sans-serif' },
	{ value: 'DejaVu Serif', label: 'DejaVu Serif', css: '"DejaVu Serif", Georgia, serif' },
	{ value: 'Liberation Serif', label: 'Liberation Serif (Times)', css: '"Liberation Serif", "Times New Roman", serif' },
	{ value: 'Liberation Mono', label: 'Liberation Mono (Courier)', css: '"Liberation Mono", "Courier New", monospace' }
];

/** CSS for a config font name (unknown names fall back to a sans-serif). */
export function fontCss(name: string | null | undefined): { family: string; weight: number } {
	const known = FONTS.find((f) => f.value === (name ?? null));
	const bold = /:bold|black|heavy/i.test(name ?? '');
	if (known) return { family: known.css, weight: bold ? 800 : 500 };
	const family = (name ?? '').split(':')[0].replace(/\.(ttf|otf)$/i, '').replace(/[-_]/g, ' ');
	return { family: `"${family}", Inter, system-ui, sans-serif`, weight: bold ? 800 : 500 };
}

/** `#RRGGBB` for a colour input from a config colour (`#RRGGBBAA`, a name, …); `fallback` when unknown. */
export function colorInputValue(c: string | null | undefined, fallback = '#ffffff'): string {
	const t = (c ?? '').trim();
	const hex = t.replace(/^(#|0x)/i, '');
	if (/^[0-9a-f]{6}([0-9a-f]{2})?$/i.test(hex)) return `#${hex.slice(0, 6).toLowerCase()}`;
	const names: Record<string, string> = { white: '#ffffff', black: '#000000', red: '#ff0000', yellow: '#ffff00', gold: '#ffd700' };
	return names[t.toLowerCase()] ?? fallback;
}

/** A CSS colour for a config colour (`#RRGGBBAA` and `name@0.5` included). */
export function cssColor(c: string | null | undefined, fallback = '#ffffff'): string {
	const t = (c ?? '').trim();
	const hex = t.replace(/^(#|0x)/i, '');
	if (/^[0-9a-f]{6}$/i.test(hex)) return `#${hex}`;
	if (/^[0-9a-f]{8}$/i.test(hex)) return `#${hex}`;
	const at = /^([a-z]+)@(0(?:\.\d+)?|1(?:\.0+)?)$/i.exec(t);
	if (at) return `color-mix(in srgb, ${at[1]} ${Math.round(Number(at[2]) * 100)}%, transparent)`;
	return /^[a-z]+$/i.test(t) ? t : fallback;
}

/** A new text line under the last one. */
export function newTextLine(after: { size?: number; y_offset?: number; position?: string; color?: string } | null) {
	const size = after?.size ? Math.max(28, Math.round(after.size * 0.55)) : 64;
	const below = after ? (after.y_offset ?? 0) + Math.round((after.size ?? 80) * 0.6 + size * 0.7) : 0;
	return {
		kind: 'text',
		content: 'New line',
		size,
		color: after?.color ?? '#ffffff',
		position: after?.position ?? 'center',
		x_offset: 0,
		y_offset: below
	};
}

/** A new opening or closing card: the vibe's own when it has one, else a plain dark card. */
export function newCard(which: 'intro' | 'outro', title: string, vibeCard?: Record<string, unknown> | null) {
	const fill = (s: unknown) =>
		typeof s === 'string' ? s.replaceAll('{title}', title).replaceAll('{trip}', title).replaceAll('{date}', '') : s;
	if (vibeCard && typeof vibeCard === 'object') {
		const card = structuredClone(vibeCard) as Record<string, unknown>;
		const layers = Array.isArray(card.layers) ? (card.layers as Record<string, unknown>[]) : [];
		card.layers = layers
			.filter((l) => l.kind === 'text')
			.map((l) => ({ ...l, content: String(fill(l.content) ?? '').trim() || title }));
		if ((card.layers as unknown[]).length > 0) return card;
	}
	return {
		duration: which === 'intro' ? '4s' : '5s',
		fade_in: '700ms',
		fade_out: '700ms',
		mode: 'card',
		bg_color: '#0a1628',
		layers: [
			{
				kind: 'text',
				content: which === 'intro' ? title : 'Thanks for watching',
				size: which === 'intro' ? 90 : 72,
				color: '#f5e6c8',
				position: 'center',
				x_offset: 0,
				y_offset: 0
			}
		]
	};
}
