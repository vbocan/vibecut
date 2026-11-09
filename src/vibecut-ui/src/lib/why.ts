// "Why this moment": turning a manifest segment's `signals` (what the
// picker measured — see the engine's moment_signals) into the moment
// inspector's words. Pure, so `npm test` covers it.

import type { PickSignal } from './types/generated/PickSignal';

/** One chip under the inspector's sentence. */
export interface WhyChip {
	kind: PickSignal['kind'];
	label: string;
	/** Tooltip / screen-reader detail: the share of the score, or what a note means. */
	detail: string;
}

/** The picker's own reasons for a moment, strongest first, as chips:
 *  "38 km/h", "airtime 0.6 s", "fast spin (458°/s)", "HiLight tag",
 *  then placement notes ("opens on a scene change"). Empty for moments
 *  the manifest records no signals for (older renders, photos, bursts). */
export function whyChips(signals: readonly PickSignal[] | null | undefined): WhyChip[] {
	return (signals ?? [])
		.filter((s) => s.label.trim() !== '')
		.map((s) => ({
			kind: s.kind,
			label: s.label,
			detail: s.share > 0 ? `${Math.round(s.share * 100)}% of this moment’s score` : 'where the clip starts'
		}));
}

/** A plain sentence for the strongest measured reason, or null when there
 *  is none worth one — on-screen motion alone keeps the inspector's
 *  generic, rank-based wording. */
export function whySentence(signals: readonly PickSignal[] | null | undefined): string | null {
	const top = (signals ?? []).find((s) => s.share > 0);
	if (!top) return null;
	const label = top.label.toLowerCase();
	switch (top.kind) {
		case 'hilight':
			return 'You marked this moment with a HiLight tag.';
		case 'landing':
			return 'Airborne — the camera felt the jump and the landing.';
		case 'gyro':
			return 'The camera spun — a flip, a spin or a hard carve.';
		case 'accel':
			return 'A big jolt — the camera felt a hit, a drop or a bump.';
		case 'gps_speed':
			if (label.startsWith('hard stop')) return 'A hard stop — the GPS caught the sudden braking.';
			if (label.startsWith('hard braking')) return 'Hard on the brakes — the GPS caught the speed dropping fast.';
			if (label.includes('top speed')) return 'The fastest stretch of this ride.';
			return 'A fast stretch — the GPS clocked the speed.';
		default:
			return null;
	}
}
