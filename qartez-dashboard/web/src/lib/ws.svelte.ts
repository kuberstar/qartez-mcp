import type { DashboardEvent } from './types';

const MAX_EVENTS = 100;
const MAX_BACKOFF_MS = 5000;
const BASE_BACKOFF_MS = 250;
const JITTER_FRACTION = 0.3;
const COUNTDOWN_INTERVAL_MS = 100;

class DashboardSocket {
	connected = $state(false);
	events = $state<DashboardEvent[]>([]);
	lastPing = $state<number | null>(null);
	reconnectIn = $state<number | null>(null);

	#ws: WebSocket | null = null;
	#attempt = 0;
	#closed = false;
	#url: string;
	#reconnectTimer: ReturnType<typeof setTimeout> | null = null;
	#countdownTimer: ReturnType<typeof setInterval> | null = null;

	constructor(url: string) {
		this.#url = url;
		this.#connect();
	}

	#connect() {
		if (this.#closed) return;
		try {
			this.#ws = new WebSocket(this.#url);
		} catch (err) {
			console.warn('[ws] failed to construct WebSocket', err);
			this.#scheduleReconnect();
			return;
		}

		this.#ws.addEventListener('open', () => {
			this.#attempt = 0;
			this.connected = true;
			this.#clearCountdown();
			this.reconnectIn = null;
		});

		this.#ws.addEventListener('close', () => {
			this.connected = false;
			this.#ws = null;
			this.#scheduleReconnect();
		});

		this.#ws.addEventListener('error', () => {
			this.connected = false;
		});

		this.#ws.addEventListener('message', (ev) => {
			let parsed: unknown;
			try {
				parsed = JSON.parse(typeof ev.data === 'string' ? ev.data : '');
			} catch (err) {
				console.warn('[ws] failed to parse message', err);
				return;
			}
			if (!parsed || typeof parsed !== 'object' || !('type' in parsed)) {
				console.warn('[ws] invalid event shape', parsed);
				return;
			}
			const evt = parsed as DashboardEvent;
			if (evt.type === 'ping') {
				this.lastPing = evt.data.ts_ms;
			}
			const next = [evt, ...this.events];
			if (next.length > MAX_EVENTS) next.length = MAX_EVENTS;
			this.events = next;
		});
	}

	#scheduleReconnect() {
		if (this.#closed) return;
		const base = Math.min(MAX_BACKOFF_MS, BASE_BACKOFF_MS * Math.pow(2, this.#attempt));
		const jitter = Math.random() * base * JITTER_FRACTION;
		const delay = base + jitter;
		this.#attempt += 1;
		if (this.#reconnectTimer !== null) clearTimeout(this.#reconnectTimer);
		this.#reconnectTimer = setTimeout(() => {
			this.#reconnectTimer = null;
			this.#connect();
		}, delay);
		this.#startCountdown(delay);
	}

	#startCountdown(delay: number) {
		this.reconnectIn = Math.max(0, Math.round(delay));
		if (this.#countdownTimer !== null) clearInterval(this.#countdownTimer);
		this.#countdownTimer = setInterval(() => {
			if (this.reconnectIn === null) {
				this.#clearCountdown();
				return;
			}
			const next = this.reconnectIn - COUNTDOWN_INTERVAL_MS;
			this.reconnectIn = next > 0 ? next : 0;
		}, COUNTDOWN_INTERVAL_MS);
	}

	#clearCountdown() {
		if (this.#countdownTimer !== null) {
			clearInterval(this.#countdownTimer);
			this.#countdownTimer = null;
		}
	}

	close() {
		this.#closed = true;
		if (this.#reconnectTimer !== null) {
			clearTimeout(this.#reconnectTimer);
			this.#reconnectTimer = null;
		}
		this.#clearCountdown();
		this.reconnectIn = null;
		this.#ws?.close();
		this.#ws = null;
	}
}

let instance: DashboardSocket | null = null;

export function dashboardSocket(): DashboardSocket {
	if (!instance) {
		const url = `ws://${location.host}/ws`;
		instance = new DashboardSocket(url);
	}
	return instance;
}

export function createDashboardSocket(url?: string): DashboardSocket {
	return new DashboardSocket(url ?? `ws://${location.host}/ws`);
}
