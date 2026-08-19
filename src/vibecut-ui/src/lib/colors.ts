// Deterministic color per source id — the same source gets the same
// hue every time the timeline re-renders, without hardcoding a fixed
// palette size (a project can have any number of sources).

export function sourceColor(sourceId: string): string {
	let hash = 0;
	for (let i = 0; i < sourceId.length; i++) {
		hash = (hash * 31 + sourceId.charCodeAt(i)) | 0;
	}
	const hue = Math.abs(hash) % 360;
	return `hsl(${hue}, 55%, 58%)`;
}
