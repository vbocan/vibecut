// Colours for sources on the timeline.
//
// The old version hashed the source id into a hue, and ids like 0101…0106
// hashed into nearly the same lime green — the timeline couldn't tell
// sources apart. Sources now take colours from a curated palette in the
// order they appear, so neighbours always differ; ids beyond the palette
// fall back to a golden-angle hue walk, which never repeats a nearby hue.

const PALETTE = [
	'#ff7a45', // ember
	'#38d6ff', // cyan
	'#ff3d81', // magenta
	'#b8f35a', // lime
	'#9b87ff', // violet
	'#ffc94a', // gold
	'#2ee6b0', // mint
	'#ff9ad5', // pink
	'#6aa8ff', // sky
	'#ffa36b' // apricot
];

/** Colour for `sourceId`, given every source id in display order. */
export function sourceColor(sourceId: string, allIds: readonly string[] = []): string {
	let i = allIds.indexOf(sourceId);
	if (i < 0) {
		// Unknown order: stable hash, still spread around the wheel.
		let hash = 0;
		for (let k = 0; k < sourceId.length; k++) hash = (hash * 31 + sourceId.charCodeAt(k)) | 0;
		i = Math.abs(hash);
	}
	if (i < PALETTE.length) return PALETTE[i];
	const hue = (i * 137.508) % 360;
	return `hsl(${hue.toFixed(0)}, 78%, 64%)`;
}
