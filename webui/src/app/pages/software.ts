// /software — the two copies of touchos, and installing an update.
import { HttpErrorResponse } from '@angular/common/http';
import { DecimalPipe } from '@angular/common';
import { Component, DestroyRef, computed, inject, signal } from '@angular/core';
import { MatButtonModule } from '@angular/material/button';
import { MatCardModule } from '@angular/material/card';
import { MatIconModule } from '@angular/material/icon';
import { MatProgressBarModule } from '@angular/material/progress-bar';
import { Subscription } from 'rxjs';
import { Phase, UpdateState } from '../api';
import { Panel } from '../panel';
import { Session } from '../session';
import { Updates } from '../update';

/** What the install is doing, for the progress card. */
type Step =
  | { kind: 'idle' }
  | { kind: 'uploading'; done: number; total: number }
  | { kind: 'checking' }
  | { kind: 'writing'; file: string; done: number; total: number }
  | { kind: 'restarting'; build: string }
  | { kind: 'done'; good: boolean; text: string };

@Component({
  selector: 'app-software',
  imports: [DecimalPipe, MatButtonModule, MatCardModule, MatIconModule, MatProgressBarModule],
  templateUrl: './software.html',
  styleUrl: './software.scss',
})
export class SoftwarePage {
  private updates = inject(Updates);
  private panel = inject(Panel);
  private session = inject(Session);

  protected readonly state = signal<UpdateState | null>(null);
  protected readonly file = signal<File | null>(null);
  protected readonly step = signal<Step>({ kind: 'idle' });
  protected readonly busy = computed(() => !['idle', 'done'].includes(this.step().kind));

  /** The two copies, A and B, for the overview. */
  protected readonly copies = computed(() => {
    const s = this.state();
    if (!s?.running) return [];
    const other = s.running === 'A' ? 'B' : 'A';
    return [
      { name: s.running, running: true, text: `build ${s.build}` },
      { name: other, running: false, text: s.other || 'unknown' },
    ].sort((a, b) => a.name.localeCompare(b.name));
  });

  private poll?: ReturnType<typeof setInterval>;
  private upload?: Subscription;

  constructor() {
    this.refresh();
    inject(DestroyRef).onDestroy(() => {
      clearInterval(this.poll);
      this.upload?.unsubscribe();
    });
  }

  protected pick(e: Event) {
    const input = e.target as HTMLInputElement;
    this.file.set(input.files?.[0] ?? null);
    input.value = '';
    if (this.step().kind === 'done') this.step.set({ kind: 'idle' });
  }

  protected size(bytes: number): string {
    return bytes > 1e6 ? `${(bytes / 1e6).toFixed(1)} MB` : `${Math.round(bytes / 1e3)} kB`;
  }

  protected percent(done: number, total: number): number {
    return total ? (100 * done) / total : 0;
  }

  private async refresh() {
    const r = await this.updates.state();
    if (r.status === 401) return this.session.lost();
    if (!r.data) return;
    this.state.set(r.data);
    this.follow(r.data.phase ?? { state: 'idle' });
  }

  /** Follow the update server while it installs. */
  private follow(p: Phase) {
    switch (p.state) {
      case 'writing':
        this.step.set({ kind: 'writing', file: p.file, done: p.done, total: p.total });
        this.poll ??= setInterval(() => this.refresh(), 1000);
        break;
      case 'restarting':
        this.stopPolling();
        this.step.set({ kind: 'restarting', build: p.build });
        this.awaitRestart(p.build);
        break;
      case 'failed':
        if (this.poll || this.busy()) {
          this.step.set({ kind: 'done', good: false, text: `The update failed: ${p.error}` });
        }
        this.stopPolling();
        break;
      case 'idle':
        break;
    }
  }

  private stopPolling() {
    clearInterval(this.poll);
    this.poll = undefined;
  }

  protected install() {
    const f = this.file();
    if (!f) return;
    this.step.set({ kind: 'uploading', done: 0, total: f.size });
    this.upload = this.updates.upload(f).subscribe({
      next: (e) => {
        if (e.kind === 'progress') {
          // All sent: the panel checks the signature before it answers.
          this.step.set(
            e.loaded >= e.total
              ? { kind: 'checking' }
              : { kind: 'uploading', done: e.loaded, total: e.total },
          );
        } else {
          this.step.set({ kind: 'writing', file: '', done: 0, total: 1 });
          this.poll ??= setInterval(() => this.refresh(), 1000);
        }
      },
      error: (e: HttpErrorResponse) => {
        if (e.status === 401) return this.session.lost();
        const why =
          e.status === 0 ? 'the connection was lost' : (e.error?.error ?? `error ${e.status}`);
        this.step.set({ kind: 'done', good: false, text: `Not installed: ${why}` });
      },
    });
  }

  /** After the restart: which build did the panel come back with? */
  private async awaitRestart(build: string) {
    if (this.panel.restarting()) return;
    const c = await this.panel.waitForComeback();
    switch (c.kind) {
      case 'back':
        this.step.set(
          c.status.build === build
            ? {
                kind: 'done',
                good: true,
                text: `The panel runs build ${build} now (copy ${c.status.slot}). It keeps it after a minute without problems.`,
              }
            : {
                kind: 'done',
                good: false,
                text: `The panel came back with build ${c.status.build}: the new copy didn't start, so the previous one is still in use.`,
              },
        );
        this.file.set(null);
        this.refresh();
        break;
      case 'unpaired':
        this.session.lost();
        break;
      case 'timeout':
        this.step.set({
          kind: 'done',
          good: false,
          text: "The panel hasn't come back after 5 minutes.",
        });
    }
  }
}
