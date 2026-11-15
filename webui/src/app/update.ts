// Software updates: upload a signed bundle; the panel checks it, writes it
// to the copy that isn't running and restarts into it once.
import { HttpClient, HttpEventType } from '@angular/common/http';
import { Injectable, inject } from '@angular/core';
import { Observable, filter, map } from 'rxjs';
import { Api, UpdateState } from './api';

export type UploadEvent =
  { kind: 'progress'; loaded: number; total: number } | { kind: 'accepted'; build: string };

@Injectable({ providedIn: 'root' })
export class Updates {
  private api = inject(Api);
  private http = inject(HttpClient);

  state() {
    return this.api.get<UpdateState>('/api/update');
  }

  /** Errors arrive as HttpErrorResponse (the body has "error"). */
  upload(file: File): Observable<UploadEvent> {
    return this.http
      .post<{ build: string }>('/api/update', file, {
        headers: { 'Content-Type': 'application/octet-stream' },
        reportProgress: true,
        observe: 'events',
      })
      .pipe(
        map((e): UploadEvent | null => {
          if (e.type === HttpEventType.UploadProgress) {
            return { kind: 'progress', loaded: e.loaded, total: e.total ?? file.size };
          }
          if (e.type === HttpEventType.Response)
            return { kind: 'accepted', build: e.body?.build ?? '?' };
          return null;
        }),
        filter((e): e is UploadEvent => e !== null),
      );
  }
}
