// The panel's status (header line, system page) and waiting for it to come
// back after a restart.
import { Injectable, computed, inject, signal } from '@angular/core';
import { Api, OFFLINE, Status } from './api';
import { Session } from './session';

export type Comeback =
  { kind: 'back'; status: Status } | { kind: 'unpaired' } | { kind: 'timeout' };

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

@Injectable({ providedIn: 'root' })
export class Panel {
  private api = inject(Api);
  private session = inject(Session);

  readonly status = signal<Status | null>(null);
  /** True while the panel doesn't answer (or is restarting). */
  readonly offline = signal(false);
  readonly restarting = signal(false);

  readonly summary = computed(() => {
    const s = this.status();
    if (this.restarting()) return 'restarting…';
    if (this.offline()) return 'not reachable';
    if (!s) return '';
    const parts = [s.ip ?? 'no network'];
    parts.push(
      s.ha.connected
        ? `Home Assistant connected (${s.ha.entities} entities)`
        : 'Home Assistant not connected',
    );
    if (s.slot) parts.push(`copy ${s.slot}` + (s.build ? ` (${s.build})` : ''));
    return parts.join(' · ');
  });

  async refresh(): Promise<Status | null> {
    const r = await this.api.get<Status>('/api/status');
    this.offline.set(r.status === OFFLINE);
    if (r.status === 401 && this.session.paired()) this.session.lost();
    if (r.data) this.status.set(r.data);
    return r.data ?? null;
  }

  /** Ask for a restart, then wait until the panel answers again. */
  async restart(): Promise<Comeback> {
    await this.api.post('/api/reboot');
    return this.waitForComeback();
  }

  /**
   * After a restart: poll until the panel answers again (up to 5 minutes).
   * A restart forgets the pairing, unless the browser pairs again.
   */
  async waitForComeback(): Promise<Comeback> {
    this.restarting.set(true);
    try {
      const t0 = Date.now();
      await sleep(8000);
      while (Date.now() - t0 < 300_000) {
        const r = await this.api.get<Status>('/api/status');
        if (r.data) {
          this.status.set(r.data);
          this.offline.set(false);
          return { kind: 'back', status: r.data };
        }
        if (r.status === 401) {
          this.offline.set(false);
          return { kind: 'unpaired' };
        }
        await sleep(3000);
      }
      return { kind: 'timeout' };
    } finally {
      this.restarting.set(false);
    }
  }
}

/** "3 d 4 h", "5 h 12 min", "42 s". */
export function duration(s: number): string {
  const d = Math.floor(s / 86400),
    h = Math.floor((s % 86400) / 3600),
    m = Math.floor((s % 3600) / 60);
  if (d) return `${d} d ${h} h`;
  if (h) return `${h} h ${m} min`;
  if (m) return `${m} min`;
  return `${s} s`;
}
