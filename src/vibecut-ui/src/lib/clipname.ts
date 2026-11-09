/** Short labels for clip ids that have to fit a narrow column. */

/**
 * `id` cut to at most `max` characters by dropping its middle: GoPro ids
 * (`0301`) stay whole, while a DJI or phone name keeps its start and its
 * distinguishing end (`DJI_20261010075256_0003_D` → `DJI_…256_0003_D`).
 */
export function shortClipName(id: string, max = 15): string {
	const chars = [...id];
	if (chars.length <= max) return id;
	const keep = max - 1; // one for the ellipsis
	const head = Math.max(1, Math.floor(keep / 3));
	const tail = keep - head;
	return `${chars.slice(0, head).join('')}…${chars.slice(chars.length - tail).join('')}`;
}
