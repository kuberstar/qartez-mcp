import * as d3 from 'd3';

export interface ForceGraphNode extends d3.SimulationNodeDatum {
	id: number;
}

export interface ForceGraphLink<N extends ForceGraphNode = ForceGraphNode>
	extends d3.SimulationLinkDatum<N> {
	source: number | N;
	target: number | N;
}

export interface ForceGraphHandle<N extends ForceGraphNode, L extends ForceGraphLink<N>> {
	simulation: d3.Simulation<N, L>;
	destroy(): void;
}

export interface CreateForceGraphOpts<N extends ForceGraphNode, L extends ForceGraphLink<N>> {
	nodes: N[];
	links: L[];
	width: number;
	height: number;
	linkDistance?: number;
	linkStrength?: number;
	chargeStrength?: number;
	collideRadius?: (node: N) => number;
	onTick: () => void;
}

export function createForceGraph<N extends ForceGraphNode, L extends ForceGraphLink<N>>(
	opts: CreateForceGraphOpts<N, L>
): ForceGraphHandle<N, L> {
	const linkForce = d3
		.forceLink<N, L>(opts.links)
		.id((d) => d.id)
		.distance(opts.linkDistance ?? 60)
		.strength(opts.linkStrength ?? 0.4);

	const sim = d3
		.forceSimulation<N, L>(opts.nodes)
		.force('link', linkForce)
		.force('charge', d3.forceManyBody().strength(opts.chargeStrength ?? -180))
		.force('center', d3.forceCenter(opts.width / 2, opts.height / 2));

	if (opts.collideRadius) {
		sim.force('collide', d3.forceCollide<N>().radius(opts.collideRadius));
	}

	sim.on('tick', opts.onTick);

	return {
		simulation: sim,
		destroy() {
			sim.on('tick', null);
			sim.stop();
		}
	};
}

export interface PaintCanvasOpts<N extends ForceGraphNode, L extends ForceGraphLink<N>> {
	ctx: CanvasRenderingContext2D;
	width: number;
	height: number;
	transform: { x: number; y: number; k: number };
	nodes: N[];
	links: L[];
	nodeRadius: (n: N) => number;
	nodeFill: (n: N) => string;
	nodeStroke?: (n: N) => string | null;
	nodeStrokeWidth?: number;
	linkStroke: (l: L) => string;
	linkOpacity?: number;
	highlightedNodeId?: number | null;
}

export function paintCanvas<N extends ForceGraphNode, L extends ForceGraphLink<N>>(
	opts: PaintCanvasOpts<N, L>
): void {
	const { ctx, width, height, transform, nodes, links } = opts;
	const dpr = typeof window !== 'undefined' ? window.devicePixelRatio || 1 : 1;
	ctx.save();
	ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
	ctx.clearRect(0, 0, width, height);
	ctx.translate(transform.x, transform.y);
	ctx.scale(transform.k, transform.k);

	ctx.globalAlpha = opts.linkOpacity ?? 0.3;
	ctx.lineWidth = 1 / transform.k;
	for (const link of links) {
		const s = link.source as N;
		const t = link.target as N;
		const sx = s.x ?? 0;
		const sy = s.y ?? 0;
		const tx = t.x ?? 0;
		const ty = t.y ?? 0;
		ctx.strokeStyle = opts.linkStroke(link);
		ctx.beginPath();
		ctx.moveTo(sx, sy);
		ctx.lineTo(tx, ty);
		ctx.stroke();
	}

	ctx.globalAlpha = 1;
	const strokeW = (opts.nodeStrokeWidth ?? 1.5) / transform.k;
	for (const node of nodes) {
		const r = opts.nodeRadius(node);
		const x = node.x ?? 0;
		const y = node.y ?? 0;
		ctx.fillStyle = opts.nodeFill(node);
		ctx.beginPath();
		ctx.arc(x, y, r, 0, Math.PI * 2);
		ctx.fill();
		const ring = opts.nodeStroke ? opts.nodeStroke(node) : null;
		const isHighlighted =
			opts.highlightedNodeId !== null &&
			opts.highlightedNodeId !== undefined &&
			node.id === opts.highlightedNodeId;
		if (ring || isHighlighted) {
			ctx.strokeStyle = ring ?? '#fbbf24';
			ctx.lineWidth = isHighlighted ? strokeW * 1.5 : strokeW;
			ctx.beginPath();
			ctx.arc(x, y, r, 0, Math.PI * 2);
			ctx.stroke();
		}
	}

	ctx.restore();
}

export function hitTestQuadtree<N extends ForceGraphNode>(
	nodes: N[],
	transform: { x: number; y: number; k: number },
	pixelX: number,
	pixelY: number,
	radius: number
): N | null {
	if (nodes.length === 0) return null;
	const wx = (pixelX - transform.x) / transform.k;
	const wy = (pixelY - transform.y) / transform.k;
	const qt = d3
		.quadtree<N>()
		.x((n) => n.x ?? 0)
		.y((n) => n.y ?? 0)
		.addAll(nodes);
	const r = radius / transform.k;
	return qt.find(wx, wy, r) ?? null;
}
