const KIND_COLORS: Record<string, string> = {
	function: 'oklch(70% 0.16 250)',
	method: 'oklch(70% 0.16 220)',
	struct: 'oklch(70% 0.18 30)',
	class: 'oklch(70% 0.18 0)',
	enum: 'oklch(72% 0.16 60)',
	trait: 'oklch(72% 0.16 130)',
	interface: 'oklch(70% 0.14 180)',
	module: 'oklch(60% 0.04 270)',
	field: 'oklch(75% 0.12 90)',
	const: 'oklch(70% 0.16 300)',
	type: 'oklch(70% 0.16 330)',
	variable: 'oklch(72% 0.10 200)'
};

const FALLBACK_KIND_COLOR = 'oklch(60% 0.02 270)';

export function kindColor(kind: string): string {
	return KIND_COLORS[kind.toLowerCase()] ?? FALLBACK_KIND_COLOR;
}

export const KIND_LEGEND: { label: string; color: string }[] = [
	{ label: 'function', color: KIND_COLORS.function },
	{ label: 'method', color: KIND_COLORS.method },
	{ label: 'struct', color: KIND_COLORS.struct },
	{ label: 'class', color: KIND_COLORS.class },
	{ label: 'enum', color: KIND_COLORS.enum },
	{ label: 'trait', color: KIND_COLORS.trait },
	{ label: 'interface', color: KIND_COLORS.interface },
	{ label: 'module', color: KIND_COLORS.module },
	{ label: 'field', color: KIND_COLORS.field },
	{ label: 'const', color: KIND_COLORS.const },
	{ label: 'type', color: KIND_COLORS.type },
	{ label: 'variable', color: KIND_COLORS.variable },
	{ label: 'other', color: FALLBACK_KIND_COLOR }
];

function hashString(s: string): number {
	let h = 2166136261;
	for (let i = 0; i < s.length; i++) {
		h ^= s.charCodeAt(i);
		h = Math.imul(h, 16777619);
	}
	return h >>> 0;
}

export function fileColor(filePath: string): string {
	const hue = hashString(filePath) % 360;
	return `oklch(70% 0.14 ${hue})`;
}
