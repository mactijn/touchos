// /system — status, the log, restart.
import {
  Component,
  DestroyRef,
  ElementRef,
  computed,
  inject,
  signal,
  viewChild,
} from '@angular/core';
import { FormsModule } from '@angular/forms';
import { MatButtonModule } from '@angular/material/button';
import { MatCardModule } from '@angular/material/card';
import { MatDialog } from '@angular/material/dialog';
import { MatIconModule } from '@angular/material/icon';
import { MatProgressBarModule } from '@angular/material/progress-bar';
import { MatSelectModule } from '@angular/material/select';
import { MatSlideToggleModule } from '@angular/material/slide-toggle';
import { MatSnackBar } from '@angular/material/snack-bar';
import { MatTooltipModule } from '@angular/material/tooltip';
import { Api, errorText } from '../api';
import { confirm } from '../confirm-dialog';
import { Panel, duration } from '../panel';
import { Session } from '../session';

/** "[   12.345] W net: …" */
interface LogLine {
  time: string;
  level: string;
  text: string;
}

@Component({
  selector: 'app-system',
  imports: [
    FormsModule,
    MatButtonModule,
    MatCardModule,
    MatIconModule,
    MatProgressBarModule,
    MatSelectModule,
    MatSlideToggleModule,
    MatTooltipModule,
  ],
  templateUrl: './system.html',
  styleUrl: './system.scss',
})
export class SystemPage {
  private api = inject(Api);
  private session = inject(Session);
  private dialog = inject(MatDialog);
  private snack = inject(MatSnackBar);
  protected panel = inject(Panel);

  protected readonly lines = signal(300);
  protected readonly log = signal<LogLine[] | null>(null);
  protected readonly loading = signal(false);
  protected readonly follow = signal(false);

  /** The status as cards: icon, label, value, ok? */
  protected readonly facts = computed(() => {
    const s = this.panel.status();
    if (!s) return [];
    return [
      {
        icon: s.ip ? 'wifi' : 'wifi_off',
        label: 'Network',
        value: s.ip ? (s.hostname ? `${s.hostname} · ${s.ip}` : s.ip) : 'no address',
        ok: !!s.ip,
      },
      {
        icon: s.ha.connected ? 'cloud_done' : 'cloud_off',
        label: 'Home Assistant',
        value: s.ha.connected ? `${s.ha.entities} entities` : 'not connected',
        ok: s.ha.connected,
      },
      {
        icon: 'deployed_code',
        label: 'Software',
        value: (s.slot ? `copy ${s.slot} · ` : '') + (s.build ?? 'unknown build'),
        ok: true,
      },
      { icon: 'timer', label: 'Running for', value: duration(s.uptime_s), ok: true },
      {
        icon: 'schedule',
        label: 'Clock',
        value: s.clock_set ? 'set from the network' : 'not set yet',
        ok: s.clock_set,
      },
      {
        icon: s.config.problems.length ? 'warning' : 'tune',
        label: 'Settings',
        value: s.config.problems.length
          ? `${s.config.problems.length} ${s.config.problems.length === 1 ? 'problem' : 'problems'}`
          : `version ${s.config.version ?? '?'}`,
        ok: !s.config.problems.length,
      },
    ];
  });

  private logBox = viewChild<ElementRef<HTMLElement>>('logBox');

  constructor() {
    this.panel.refresh();
    this.loadLog();
    const t = setInterval(() => {
      if (this.follow() && document.visibilityState === 'visible') this.loadLog();
    }, 3000);
    inject(DestroyRef).onDestroy(() => clearInterval(t));
  }

  protected async loadLog() {
    const box = this.logBox()?.nativeElement;
    const atBottom = !box || box.scrollHeight - box.scrollTop - box.clientHeight < 40;
    this.loading.set(true);
    const r = await this.api.getText(`/api/log?n=${this.lines()}`);
    this.loading.set(false);
    if (r.status === 401) return this.session.lost();
    if (r.data === undefined) {
      this.snack.open(errorText(r, "Couldn't read the log"), 'OK', { panelClass: 'snack-bad' });
      return;
    }
    this.log.set(
      r.data
        .split('\n')
        .filter((l) => l)
        .map(parseLine),
    );
    // Newest at the bottom: stay there unless the user scrolled up.
    if (atBottom) {
      setTimeout(() => {
        const el = this.logBox()?.nativeElement;
        if (el) el.scrollTop = el.scrollHeight;
      });
    }
  }

  protected async restart() {
    const ok = await confirm(this.dialog, {
      title: 'Restart the panel?',
      text: 'It starts its active copy of the system again. This browser has to pair again afterwards.',
      ok: 'Restart',
      danger: true,
    });
    if (!ok) return;
    const c = await this.panel.restart();
    switch (c.kind) {
      case 'back':
        this.snack.open(
          `The panel is back (copy ${c.status.slot ?? '?'}, build ${c.status.build ?? '?'}).`,
          'OK',
          {
            duration: 8000,
          },
        );
        break;
      case 'unpaired':
        this.session.lost();
        break;
      case 'timeout':
        this.snack.open("The panel hasn't come back after 5 minutes.", 'OK', {
          panelClass: 'snack-bad',
        });
    }
  }
}

function parseLine(l: string): LogLine {
  const m = /^\[\s*([\d.]+)\]\s+([A-Z])\s(.*)$/.exec(l);
  return m ? { time: m[1], level: m[2], text: m[3] } : { time: '', level: '', text: l };
}
