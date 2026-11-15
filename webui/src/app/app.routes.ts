import { Routes } from '@angular/router';
import { PairPage } from './pages/pair';
import { SectionPage } from './pages/section';
import { SoftwarePage } from './pages/software';
import { SystemPage } from './pages/system';
import { TilesPage } from './pages/tiles';
import { pairedGuard, unpairedGuard } from './session';

// All pages load with the app: the panel serves one request per connection,
// so one bigger file beats several small ones.
export const routes: Routes = [
  { path: 'pair', title: 'Pair · touchos', canActivate: [unpairedGuard], component: PairPage },
  {
    path: '',
    canActivateChild: [pairedGuard],
    children: [
      { path: '', pathMatch: 'full', redirectTo: 'settings/homeassistant' },
      { path: 'settings', pathMatch: 'full', redirectTo: 'settings/homeassistant' },
      { path: 'settings/:section', title: 'Settings · touchos', component: SectionPage },
      { path: 'tiles', title: 'Tiles · touchos', component: TilesPage },
      { path: 'software', title: 'Software · touchos', component: SoftwarePage },
      { path: 'system', title: 'System · touchos', component: SystemPage },
    ],
  },
  { path: '**', redirectTo: '' },
];
