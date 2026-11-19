// The System page's "Processes" card: like `top` for the panel. Polls
// every 3 s while the page is visible; rates come from two polls.
import { Component, DestroyRef, computed, inject, signal } from '@angular/core';
import { MatCardModule } from '@angular/material/card';
import { MatIconModule } from '@angular/material/icon';
import { MatTooltipModule } from '@angular/material/tooltip';
import { Processes } from '../api';
import { Panel, duration } from '../panel';
import { SortKey, bytes, pushHistory, rows, sorted, sparkline } from '../processes';

/** Points in the CPU sparkline: 5 minutes at 3 s. */
const HISTORY = 100;
const EVERY_MS = 3000;

@Component({
  selector: 'app-processes-card',
  imports: [MatCardModule, MatIconModule, MatTooltipModule],
  templateUrl: './processes-card.html',
  styleUrl: './processes-card.scss',
})
export class ProcessesCard {
  private panel = inject(Panel);

  private readonly cur = signal<Processes | null>(null);
  private readonly prev = signal<Processes | null>(null);
  protected readonly unavailable = signal(false);
  protected readonly sortKey = signal<SortKey>('cpu');
  protected readonly desc = signal(false);
  protected readonly history = signal<number[]>([]);

  protected readonly view = computed(() => {
    const c = this.cur();
    return c ? rows(c, this.prev()) : null;
  });
  protected readonly list = computed(() => {
    const v = this.view();
    return v ? sorted(v.processes, this.sortKey(), this.desc()) : [];
  });
  protected readonly spark = computed(() => sparkline(this.history(), 120, 32, HISTORY));

  protected readonly columns: { key: SortKey; label: string; num: boolean; tip: string }[] = [
    { key: 'name', label: 'Process', num: false, tip: 'One thread each; "idle" is the CPU waiting' },
    { key: 'pid', label: 'PID', num: true, tip: 'Process id (kernel threads have none)' },
    { key: 'cpu', label: 'CPU', num: true, tip: 'Share of the CPU since the last refresh' },
    { key: 'switchesPerS', label: 'Switches/s', num: true, tip: 'Times it was switched to, per second' },
    {
      key: 'memory',
      label: 'Memory',
      num: true,
      tip: 'Image, stack and heap; not shared memory. Never given back, so it only grows',
    },
    { key: 'restarts', label: 'Restarts', num: true, tip: 'Restarts since boot (crashes in brackets)' },
    { key: 'status', label: 'State', num: false, tip: 'What it is doing, or why it isn\'t running' },
  ];

  protected readonly bytes = bytes;
  protected readonly duration = duration;

  constructor() {
    void this.poll();
    const t = setInterval(() => {
      if (document.visibilityState === 'visible') void this.poll();
    }, EVERY_MS);
    // Back on the tab: the next difference would span the hidden time, so
    // start the rates over.
    const onVisible = () => {
      if (document.visibilityState === 'visible') {
        this.cur.set(null);
        void this.poll();
      }
    };
    document.addEventListener('visibilitychange', onVisible);
    inject(DestroyRef).onDestroy(() => {
      clearInterval(t);
      document.removeEventListener('visibilitychange', onVisible);
    });
  }

  private async poll() {
    const p = await this.panel.processes();
    if (!p) {
      this.unavailable.set(true);
      return;
    }
    this.unavailable.set(false);
    this.prev.set(this.cur());
    this.cur.set(p);
    const busy = this.view()?.system.busy;
    if (busy !== null && busy !== undefined) this.history.update((h) => pushHistory(h, busy, HISTORY));
  }

  protected sortBy(key: SortKey) {
    if (this.sortKey() === key) this.desc.update((d) => !d);
    else {
      this.sortKey.set(key);
      this.desc.set(false);
    }
  }

  protected pct(v: number | null): string {
    return v === null ? '' : `${v < 10 ? v.toFixed(1) : Math.round(v)} %`;
  }

  protected rate(v: number | null): string {
    return v === null ? '' : v < 10 ? v.toFixed(1) : String(Math.round(v));
  }

  protected heap(): string {
    const s = this.view()?.system;
    return s ? `${bytes(s.heapUsed)} of ${bytes(s.heapSize)}` : '';
  }
}
