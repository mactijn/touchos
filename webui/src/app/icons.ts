// The icons the app uses: Material Symbols (outlined) as inline SVGs,
// bundled into the script. No icon font: the page must work without the
// internet, and the panel serves one request per connection.
import { inject } from '@angular/core';
import { MatIconRegistry } from '@angular/material/icon';
import { DomSanitizer } from '@angular/platform-browser';

import iconMenu from '@material-symbols/svg-400/outlined/menu.svg';
import iconLogout from '@material-symbols/svg-400/outlined/logout.svg';
import iconTune from '@material-symbols/svg-400/outlined/tune.svg';
import iconHome from '@material-symbols/svg-400/outlined/home.svg';
import iconWifi from '@material-symbols/svg-400/outlined/wifi.svg';
import iconWifiOff from '@material-symbols/svg-400/outlined/wifi_off.svg';
import iconBrightnessMedium from '@material-symbols/svg-400/outlined/brightness_medium.svg';
import iconSpaceDashboard from '@material-symbols/svg-400/outlined/space_dashboard.svg';
import iconSchedule from '@material-symbols/svg-400/outlined/schedule.svg';
import iconLanguage from '@material-symbols/svg-400/outlined/language.svg';
import iconBugReport from '@material-symbols/svg-400/outlined/bug_report.svg';
import iconGridView from '@material-symbols/svg-400/outlined/grid_view.svg';
import iconSystemUpdateAlt from '@material-symbols/svg-400/outlined/system_update_alt.svg';
import iconMonitorHeart from '@material-symbols/svg-400/outlined/monitor_heart.svg';
import iconDragIndicator from '@material-symbols/svg-400/outlined/drag_indicator.svg';
import iconDelete from '@material-symbols/svg-400/outlined/delete.svg';
import iconAdd from '@material-symbols/svg-400/outlined/add.svg';
import iconCheckCircle from '@material-symbols/svg-400/outlined/check_circle.svg';
import iconError from '@material-symbols/svg-400/outlined/error.svg';
import iconWarning from '@material-symbols/svg-400/outlined/warning.svg';
import iconCloudDone from '@material-symbols/svg-400/outlined/cloud_done.svg';
import iconCloudOff from '@material-symbols/svg-400/outlined/cloud_off.svg';
import iconRestartAlt from '@material-symbols/svg-400/outlined/restart_alt.svg';
import iconUploadFile from '@material-symbols/svg-400/outlined/upload_file.svg';
import iconRefresh from '@material-symbols/svg-400/outlined/refresh.svg';
import iconLock from '@material-symbols/svg-400/outlined/lock.svg';
import iconSync from '@material-symbols/svg-400/outlined/sync.svg';
import iconInfo from '@material-symbols/svg-400/outlined/info.svg';
import iconClose from '@material-symbols/svg-400/outlined/close.svg';
import iconLan from '@material-symbols/svg-400/outlined/lan.svg';
import iconPassword from '@material-symbols/svg-400/outlined/password.svg';
import iconTimer from '@material-symbols/svg-400/outlined/timer.svg';
import iconDeployedCode from '@material-symbols/svg-400/outlined/deployed_code.svg';
import iconMonitoring from '@material-symbols/svg-400/outlined/monitoring.svg';

const ICONS: Record<string, string> = {
  menu: iconMenu,
  logout: iconLogout,
  tune: iconTune,
  home: iconHome,
  wifi: iconWifi,
  wifi_off: iconWifiOff,
  brightness_medium: iconBrightnessMedium,
  space_dashboard: iconSpaceDashboard,
  schedule: iconSchedule,
  language: iconLanguage,
  bug_report: iconBugReport,
  grid_view: iconGridView,
  system_update_alt: iconSystemUpdateAlt,
  monitor_heart: iconMonitorHeart,
  drag_indicator: iconDragIndicator,
  delete: iconDelete,
  add: iconAdd,
  check_circle: iconCheckCircle,
  error: iconError,
  warning: iconWarning,
  cloud_done: iconCloudDone,
  cloud_off: iconCloudOff,
  restart_alt: iconRestartAlt,
  upload_file: iconUploadFile,
  refresh: iconRefresh,
  lock: iconLock,
  sync: iconSync,
  info: iconInfo,
  close: iconClose,
  lan: iconLan,
  password: iconPassword,
  timer: iconTimer,
  deployed_code: iconDeployedCode,
  monitoring: iconMonitoring,
};

/** Register them for <mat-icon svgIcon="name">. */
export function registerIcons() {
  const registry = inject(MatIconRegistry);
  const sanitizer = inject(DomSanitizer);
  for (const [name, svg] of Object.entries(ICONS)) {
    // Our own files from node_modules, not user input.
    registry.addSvgIconLiteral(name, sanitizer.bypassSecurityTrustHtml(svg));
  }
}
