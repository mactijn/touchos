// /pair — get a session: the panel shows a code, the user types it here.
import {
  Component,
  DestroyRef,
  ElementRef,
  computed,
  inject,
  input,
  signal,
  viewChild,
} from '@angular/core';
import { FormsModule } from '@angular/forms';
import { MatButtonModule } from '@angular/material/button';
import { MatCardModule } from '@angular/material/card';
import { MatFormFieldModule } from '@angular/material/form-field';
import { MatIconModule } from '@angular/material/icon';
import { MatInputModule } from '@angular/material/input';
import { MatProgressBarModule } from '@angular/material/progress-bar';
import { Router } from '@angular/router';
import { errorText } from '../api';
import { Session } from '../session';

@Component({
  selector: 'app-pair',
  imports: [
    FormsModule,
    MatButtonModule,
    MatCardModule,
    MatFormFieldModule,
    MatIconModule,
    MatInputModule,
    MatProgressBarModule,
  ],
  templateUrl: './pair.html',
  styleUrl: './pair.scss',
})
export class PairPage {
  private session = inject(Session);
  private router = inject(Router);

  /** Where to go after pairing (?next=…). */
  readonly next = input<string>();

  protected readonly pin = signal('');
  protected readonly busy = signal(false);
  protected readonly error = signal<string | null>(null);
  /** When the code on the panel's screen expires (ms since epoch); 0: none asked for. */
  private readonly expires = signal(0);
  private readonly ttl = signal(1);
  private readonly now = signal(Date.now());
  protected readonly secondsLeft = computed(() =>
    Math.max(0, Math.ceil((this.expires() - this.now()) / 1000)),
  );
  protected readonly asking = computed(() => this.secondsLeft() > 0);
  protected readonly expired = computed(() => this.expires() > 0 && !this.asking());
  protected readonly left = computed(() => (100 * this.secondsLeft()) / this.ttl());

  private pinInput = viewChild<ElementRef<HTMLInputElement>>('pinInput');

  constructor() {
    const t = setInterval(() => this.now.set(Date.now()), 1000);
    inject(DestroyRef).onDestroy(() => clearInterval(t));
  }

  async start() {
    this.busy.set(true);
    this.error.set(null);
    const r = await this.session.startPairing();
    this.busy.set(false);
    if (r.data) {
      this.pin.set('');
      this.ttl.set(r.data.expires_in);
      this.now.set(Date.now());
      this.expires.set(Date.now() + r.data.expires_in * 1000);
      setTimeout(() => this.pinInput()?.nativeElement.focus());
    } else {
      this.error.set(errorText(r));
    }
  }

  async submit() {
    const pin = this.pin().replace(/\s/g, '');
    if (!/^[0-9]{6}$/.test(pin)) {
      this.error.set('The code has 6 digits.');
      return;
    }
    this.busy.set(true);
    const r = await this.session.pair(pin);
    this.busy.set(false);
    if (r.data?.paired) {
      const next = this.next();
      this.router.navigateByUrl(
        next && next.startsWith('/') && !next.startsWith('//') ? next : '/',
      );
    } else if (r.status === 403) {
      this.pin.set('');
      this.error.set(`Wrong code: ${r.err?.['tries_left']} tries left.`);
    } else {
      this.expires.set(0);
      this.error.set(errorText(r));
    }
  }
}
