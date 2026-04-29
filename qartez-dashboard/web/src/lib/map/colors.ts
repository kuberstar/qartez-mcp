// Hot ramp + cluster palette for the map page.
// All values are OKLCH triples interpolated in OKLCH space; the output is a
// CSS oklch(...) string that the d3 fill attribute can consume directly.
// We avoid d3-scale-chromatic (3-5 KB gzipped) by hand-rolling a 5-stop ramp.

interface OklchTriple {
	l: number;
	c: number;
	h: number;
}

interface RampStop {
	t: number;
	color: OklchTriple;
}

const HOT_STOPS: RampStop[] = [
	{ t: 0.0, color: { l: 60, c: 0.1, h: 250 } },
	{ t: 0.25, color: { l: 65, c: 0.12, h: 200 } },
	{ t: 0.5, color: { l: 80, c: 0.14, h: 95 } },
	{ t: 0.75, color: { l: 70, c: 0.18, h: 60 } },
	{ t: 1.0, color: { l: 60, c: 0.22, h: 30 } }
];

function lerp(a: number, b: number, t: number): number {
	return a + (b - a) * t;
}

function lerpHue(a: number, b: number, t: number): number {
	// Take the shortest arc on the hue circle.
	const diff = ((b - a + 540) % 360) - 180;
	return (a + diff * t + 360) % 360;
}

function fmt(c: OklchTriple): string {
	return `oklch(${c.l.toFixed(2)}% ${c.c.toFixed(3)} ${c.h.toFixed(1)})`;
}

export function hotColor(score: number): string {
	const t = Number.isFinite(score) ? Math.min(1, Math.max(0, score)) : 0;
	for (let i = 0; i < HOT_STOPS.length - 1; i++) {
		const a = HOT_STOPS[i];
		const b = HOT_STOPS[i + 1];
		if (t <= b.t) {
			const span = b.t - a.t;
			const local = span === 0 ? 0 : (t - a.t) / span;
			return fmt({
				l: lerp(a.color.l, b.color.l, local),
				c: lerp(a.color.c, b.color.c, local),
				h: lerpHue(a.color.h, b.color.h, local)
			});
		}
	}
	return fmt(HOT_STOPS[HOT_STOPS.length - 1].color);
}

export const HOT_LEGEND_STOPS: { label: string; color: string }[] = [
	{ label: 'low', color: hotColor(0) },
	{ label: '', color: hotColor(0.25) },
	{ label: '', color: hotColor(0.5) },
	{ label: '', color: hotColor(0.75) },
	{ label: 'high', color: hotColor(1) }
];

const CLUSTER_PALETTE: string[] = [
	'oklch(70% 0.16 0)',
	'oklch(70% 0.16 36)',
	'oklch(75% 0.14 72)',
	'oklch(72% 0.16 130)',
	'oklch(70% 0.14 180)',
	'oklch(70% 0.14 220)',
	'oklch(65% 0.16 260)',
	'oklch(70% 0.18 300)',
	'oklch(70% 0.18 330)',
	'oklch(65% 0.04 270)'
];

export function clusterColor(clusterId: number): string {
	const idx = ((clusterId % CLUSTER_PALETTE.length) + CLUSTER_PALETTE.length) % CLUSTER_PALETTE.length;
	return CLUSTER_PALETTE[idx];
}
