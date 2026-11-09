/** The viewer asked their system for less motion. app.css already stills
 *  CSS animations for them; this covers motion started from script —
 *  smooth scrolling and the finished reel playing by itself. */
export function prefersReducedMotion(): boolean {
	return typeof matchMedia === 'function' && matchMedia('(prefers-reduced-motion: reduce)').matches;
}
