// Pairing: the panel shows a PIN on its screen; typing it here gets a
// session cookie (HttpOnly, so all we know is what /api/session says).
import { Injectable, inject, signal } from '@angular/core';
import { CanActivateFn, Router } from '@angular/router';
import { Api, Reply } from './api';

@Injectable({ providedIn: 'root' })
export class Session {
  private api = inject(Api);
  private router = inject(Router);

  /** null until the panel has been asked. */
  readonly paired = signal<boolean | null>(null);

  async check(): Promise<boolean> {
    const r = await this.api.get<{ paired: boolean }>('/api/session');
    const paired = r.data?.paired ?? false;
    this.paired.set(paired);
    return paired;
  }

  /** Ask the panel to show a PIN. */
  startPairing() {
    return this.api.post<{ expires_in: number }>('/api/pair/start');
  }

  async pair(pin: string): Promise<Reply<{ paired: boolean }>> {
    const r = await this.api.post<{ paired: boolean }>('/api/pair', { pin });
    if (r.data?.paired) this.paired.set(true);
    return r;
  }

  async logout() {
    await this.api.post('/api/logout');
    this.lost();
  }

  /** The panel no longer knows us (signed out, restarted, expired). */
  lost() {
    this.paired.set(false);
    const next = this.router.url.startsWith('/pair') ? undefined : this.router.url;
    this.router.navigate(['/pair'], { queryParams: next ? { next } : {} });
  }
}

/** Pages behind pairing. */
// (inject() only works before the first await.)
export const pairedGuard: CanActivateFn = async (_route, state) => {
  const session = inject(Session);
  const router = inject(Router);
  const paired = session.paired() ?? (await session.check());
  return paired || router.createUrlTree(['/pair'], { queryParams: { next: state.url } });
};

/** The pairing page, only while not paired. */
export const unpairedGuard: CanActivateFn = async () => {
  const session = inject(Session);
  const router = inject(Router);
  const paired = await session.check();
  return !paired || router.createUrlTree(['/settings']);
};
