// The frame: navigation (a side bar, or a drawer on phones), the status in
// the top bar, and the save bar while there are unsaved changes.
import { BreakpointObserver } from '@angular/cdk/layout';
import { Component, DestroyRef, computed, effect, inject, viewChild } from '@angular/core';
import { toSignal } from '@angular/core/rxjs-interop';
import { MatBadgeModule } from '@angular/material/badge';
import { MatButtonModule } from '@angular/material/button';
import { MatDialog } from '@angular/material/dialog';
import { MatDividerModule } from '@angular/material/divider';
import { MatIconModule } from '@angular/material/icon';
import { MatListModule } from '@angular/material/list';
import { MatProgressBarModule } from '@angular/material/progress-bar';
import { MatSidenav, MatSidenavModule } from '@angular/material/sidenav';
import { MatSnackBar } from '@angular/material/snack-bar';
import { MatToolbarModule } from '@angular/material/toolbar';
import { MatTooltipModule } from '@angular/material/tooltip';
import { Router, RouterLink, RouterLinkActive, RouterOutlet } from '@angular/router';
import { map } from 'rxjs';
import { Config } from './config';
import { confirm } from './confirm-dialog';
import { Panel } from './panel';
import { Session } from './session';
import { SettingsForm } from './settings-form';

/** Status refresh while paired and the tab is visible. */
const STATUS_EVERY_MS = 15_000;

@Component({
  selector: 'app-root',
  imports: [
    RouterOutlet,
    RouterLink,
    RouterLinkActive,
    MatBadgeModule,
    MatButtonModule,
    MatDividerModule,
    MatIconModule,
    MatListModule,
    MatProgressBarModule,
    MatSidenavModule,
    MatToolbarModule,
    MatTooltipModule,
  ],
  templateUrl: './app.html',
  styleUrl: './app.scss',
  host: { '(window:beforeunload)': 'onUnload($event)' },
})
export class App {
  protected session = inject(Session);
  protected panel = inject(Panel);
  protected config = inject(Config);
  protected form = inject(SettingsForm);
  private router = inject(Router);
  private snack = inject(MatSnackBar);
  private dialog = inject(MatDialog);

  protected readonly wide = toSignal(
    inject(BreakpointObserver)
      .observe('(min-width: 900px)')
      .pipe(map((s) => s.matches)),
    { initialValue: true },
  );
  private nav = viewChild<MatSidenav>('nav');

  protected readonly pages = [
    { path: '/tiles', label: 'Tiles', icon: 'grid_view' },
    { path: '/software', label: 'Software', icon: 'system_update_alt' },
    { path: '/system', label: 'System', icon: 'monitor_heart' },
  ];

  protected readonly haOk = computed(() => this.panel.status()?.ha.connected ?? false);

  constructor() {
    let timer: ReturnType<typeof setInterval> | undefined;
    effect(() => {
      clearInterval(timer);
      if (!this.session.paired()) return;
      this.panel.refresh();
      this.config.load();
      timer = setInterval(() => {
        if (document.visibilityState === 'visible' && !this.panel.restarting())
          this.panel.refresh();
      }, STATUS_EVERY_MS);
    });
    inject(DestroyRef).onDestroy(() => clearInterval(timer));
  }

  protected navigated() {
    if (!this.wide()) this.nav()?.close();
  }

  protected onUnload(e: BeforeUnloadEvent) {
    if (this.form.count() > 0) e.preventDefault();
  }

  protected async save() {
    const r = await this.form.save();
    if (r.ok) {
      this.snack.open(r.message, 'OK', {
        duration: r.warn ? undefined : 6000,
        panelClass: r.warn ? 'snack-bad' : undefined,
      });
    } else {
      this.snack.open(r.message, 'OK', { panelClass: 'snack-bad' });
      if (r.section)
        this.router.navigate(r.section === 'tiles' ? ['/tiles'] : ['/settings', r.section]);
    }
  }

  protected async signOut() {
    if (
      this.form.count() > 0 &&
      !(await confirm(this.dialog, {
        title: 'Sign out?',
        text: 'Your unsaved changes will be lost.',
        ok: 'Sign out',
        danger: true,
      }))
    ) {
      return;
    }
    await this.session.logout();
  }
}
