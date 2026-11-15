//! The wall-panel dashboard: clock + status bar and pages of Home Assistant
//! tiles. Tap to toggle, drag sideways on a dimmable light to set its
//! brightness, use -/+ on thermostats, swipe sideways between pages.

use ::log::info;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use touchos_proto::ha::CallData;

use crate::backlight::Backlight;
use crate::gfx::{self, Canvas, Color, fonts as f};
use crate::{Framebuffer, config, events, ha, input, net, time};
use touchos_rt::exec;

mod gesture;
mod pin;
mod power;
mod render;
mod screen;
mod theme;
mod tiles;

use pin::*;
use power::*;
use screen::*;
use theme::*;
use tiles::*;

struct Tile {
    entity: String,
    name: Option<String>,
    /// Area (room) id, if the entity or its device is assigned to one.
    area: Option<String>,
}

#[derive(Clone, PartialEq)]
enum RoomKey {
    /// No rooms known: everything.
    All,
    /// Entities without an area.
    Other,
    Area(String),
}

#[derive(Clone, Copy, PartialEq)]
enum Gesture {
    None,
    /// Finger down, not yet decided: (start x, start y, tile index on page)
    Pending(i32, i32, Option<usize>),
    /// Dragging the brightness of a light tile: (tile index, start x, start brightness)
    Dim(usize, i32, i32),
    Swipe(i32),
    /// Finger down on a room button.
    Chip(usize),
}

struct Ui {
    canvas: Canvas,
    tiles: Vec<Tile>,
    /// Rooms that have tiles: (area id or None for "Other", display name).
    rooms: Vec<(RoomKey, String)>,
    room: usize,
    /// Indices into `tiles` for the selected room.
    visible: Vec<usize>,
    chip_rects: Vec<(i32, i32, i32, i32)>,
    /// Top of the tile grid (below the room buttons).
    top: i32,
    /// Baseline of the status text (top right, or below the date if it doesn't fit).
    status_y: i32,
    page: usize,
    cols: i32,
    rows: i32,
    /// Tile height: TILE_H, or less to fit a second row on a small screen.
    tile_h: i32,
    pointer: (i32, i32),
    pointer_seen: u64,
    /// Whether the last frame has the pointer dot.
    pointer_drawn: bool,
    buttons: u8,
    gesture: Gesture,
    /// Brightness preview while dragging: (tile index, brightness 0..=255)
    preview: Option<(usize, i32)>,
    pressed: Option<(usize, u64)>,
    utc_offset: i64,
    eu_dst: bool,
    rotation: u8,
}

impl Ui {
    fn per_page(&self) -> usize {
        (self.cols * self.rows) as usize
    }
    fn pages(&self) -> usize {
        self.visible.len().div_ceil(self.per_page()).max(1)
    }

    /// Lay out room buttons (wrapping) and compute where the grid starts.
    fn layout_chips(&mut self) {
        self.chip_rects.clear();
        // Does the status fit to the right of the clock and date? Measure the widest cases.
        let header_w = PAD
            + s(8)
            + f::CLOCK.width("00:00")
            + s(36)
            + f::TEXT_BOLD
                .width("Wednesday")
                .max(f::TEXT.width("30 September"));
        let status_w = s(48) + f::TEXT_SMALL.width("Waiting for network…");
        let fits = header_w + s(32) + status_w <= self.canvas.w - PAD - s(8);
        self.status_y = if fits { s(70) } else { s(178) };
        let chips_y = if fits { CHIPS_Y } else { CHIPS_Y + s(40) };
        let (mut x, mut y) = (PAD, chips_y);
        if self.rooms.len() > 1 {
            for (_, name) in &self.rooms {
                let w = f::TEXT.width(name) + 2 * M;
                if x + w > self.canvas.w - PAD && x > PAD {
                    x = PAD;
                    y += CHIP_H + s(12);
                }
                self.chip_rects.push((x, y, w, CHIP_H));
                x += w + s(12);
            }
            self.top = y + CHIP_H + PAD;
        } else {
            self.top = TOP;
        }
    }

    fn select_room(&mut self, r: usize) {
        self.room = r.min(self.rooms.len().saturating_sub(1));
        let key = self
            .rooms
            .get(self.room)
            .map_or(RoomKey::All, |r| r.0.clone());
        self.visible = (0..self.tiles.len())
            .filter(|&i| match &key {
                RoomKey::All => true,
                RoomKey::Other => self.tiles[i].area.is_none(),
                RoomKey::Area(a) => self.tiles[i].area.as_ref() == Some(a),
            })
            .collect();
        self.page = 0;
        self.preview = None;
    }

    fn layout(&mut self, tiles: Vec<Tile>, rooms: Vec<(RoomKey, String)>) {
        let (w, h) = (self.canvas.w, self.canvas.h);
        self.cols = if w >= 1600 {
            4
        } else if w >= 1000 {
            3
        } else {
            2
        };
        let keep = self.rooms.get(self.room).map(|r| r.0.clone());
        self.tiles = tiles;
        self.rooms = rooms;
        self.layout_chips();
        let avail = h - self.top - DOTS_H;
        self.rows = (avail / (TILE_H + PAD)).max(1);
        self.tile_h = TILE_H;
        // Two rows of somewhat smaller tiles beat one row (800x480 with two
        // rows of room buttons).
        if self.rows < 2 && avail / 2 - PAD >= TILE_H * 3 / 4 {
            self.rows = 2;
            self.tile_h = avail / 2 - PAD;
        }
        let r = keep
            .and_then(|k| self.rooms.iter().position(|r| r.0 == k))
            .unwrap_or(0);
        self.select_room(r);
    }

    /// Rectangle of the i-th tile on the current page.
    fn rect(&self, i: usize) -> (i32, i32, i32, i32) {
        let tile_w = (self.canvas.w - PAD * (self.cols + 1)) / self.cols;
        let (r, c) = (i as i32 / self.cols, i as i32 % self.cols);
        (
            PAD + c * (tile_w + PAD),
            self.top + r * (self.tile_h + PAD),
            tile_w,
            self.tile_h,
        )
    }

    /// Indices (into `tiles`) of the tiles on the current page.
    fn page_tiles(&self) -> Vec<usize> {
        let start = self.page * self.per_page();
        self.visible
            .iter()
            .skip(start)
            .take(self.per_page())
            .copied()
            .collect()
    }

    fn hit(&self, x: i32, y: i32) -> Option<usize> {
        let n = self.page_tiles().len();
        (0..n).find(|&i| {
            let (tx, ty, tw, th) = self.rect(i);
            x >= tx && x < tx + tw && y >= ty && y < ty + th
        })
    }

    fn hit_chip(&self, x: i32, y: i32) -> Option<usize> {
        self.chip_rects
            .iter()
            .position(|&(cx, cy, cw, ch)| x >= cx && x < cx + cw && y >= cy && y < cy + ch)
    }

    fn entity_of(&self, i: usize) -> Option<ha::Entity> {
        let idx = *self.page_tiles().get(i)?;
        ha::STATE
            .lock()
            .entities
            .get(&self.tiles[idx].entity)
            .cloned()
    }

    /// The entities on the current page, as `render` shows them.
    fn shown(&self, st: &ha::HaState) -> Vec<Option<ha::Entity>> {
        self.page_tiles()
            .into_iter()
            .map(|idx| st.entities.get(&self.tiles[idx].entity).cloned())
            .collect()
    }

    /// The pointer (touchpad) dot is drawn while in use. (0: never used. A
    /// fast boot draws its first frames within 5 s.)
    fn pointer_visible(&self) -> bool {
        self.pointer_seen != 0 && time::now_ms() - self.pointer_seen < 5000
    }


}

pub async fn task(mut fb: Framebuffer) {
    let cfg = config::get();
    let mut ui = Ui {
        canvas: Canvas::new(fb.width as i32, fb.height as i32),
        tiles: Vec::new(),
        rooms: Vec::new(),
        room: 0,
        visible: Vec::new(),
        chip_rects: Vec::new(),
        top: TOP,
        status_y: s(70),
        page: 0,
        cols: 4,
        rows: 3,
        tile_h: TILE_H,
        pointer: (fb.width as i32 / 2, fb.height as i32 / 2),
        pointer_seen: 0,
        pointer_drawn: false,
        buttons: 0,
        gesture: Gesture::None,
        preview: None,
        pressed: None,
        utc_offset: cfg.display.utc_offset_minutes as i64,
        eu_dst: cfg.display.eu_dst,
        rotation: 0,
    };
    info!("dashboard on {}x{}", fb.width, fb.height);
    let mut screen = Screen::new();
    let (pw, ph) = (fb.width as i32, fb.height as i32);
    let mut drawn_version = u64::MAX;
    let mut drawn_minute = u64::MAX;
    // What the last frame showed of the outside world: the entities on the
    // page and (HA connected, network address).
    let mut drawn_entities: Vec<Option<ha::Entity>> = Vec::new();
    let mut drawn_status = (false, false);
    let mut tiles_for_registry: Option<u64> = None;
    let mut dirty = true;
    // A touch that only woke the screen is ignored until the finger lifts.
    let mut swallow = false;
    let mut shown_pin: Option<(String, u64)> = None;
    // The power button: when it went down (until it counts as a long press),
    // and the menu a long press opens.
    let mut power_down_at: Option<u64> = None;
    let mut menu: Option<PowerMenu> = None;
    let mut short_press = false;
    loop {
        ha::refresh();
        let pin = crate::overlay::current();
        if pin.is_some() {
            // Keep the screen on (also at night) while a PIN must be read.
            screen.activity();
        }
        if pin != shown_pin {
            shown_pin = pin;
            dirty = true;
        }
        if config::changed() {
            // Display settings, time zone, tiles and rooms: rebuild.
            let cfg = config::get();
            ui.utc_offset = cfg.display.utc_offset_minutes as i64;
            ui.eu_dst = cfg.display.eu_dst;
            screen = Screen::new_from(screen);
            tiles_for_registry = None;
            dirty = true;
        }
        while let Some(ev) = input::pop() {
            dirty = true;
            match ev {
                // A keyboard's Power key (HID usage 0x66) works like the button.
                input::Event::Key {
                    usage: 0x66, down, ..
                } => {
                    if down {
                        power_down_at.get_or_insert(time::now_ms());
                    } else if power_down_at.take().is_some() {
                        short_press = true;
                    }
                }
                input::Event::PowerButton { down: true } => {
                    power_down_at = Some(time::now_ms());
                }
                input::Event::PowerButton { down: false } => {
                    short_press |= power_down_at.take().is_some();
                }
                // The power menu takes every touch: a button, or outside = cancel.
                input::Event::TouchDown { x, y } if menu.is_some() => {
                    swallow = true;
                    screen.activity();
                    let (lx, ly) = gfx::panel_to_logical(ui.rotation, pw, ph, x, y);
                    menu = menu_tap(menu, ui.canvas.w, ui.canvas.h, lx, ly);
                }
                // A tap anywhere hides the PIN.
                input::Event::TouchDown { .. } if shown_pin.is_some() => {
                    crate::overlay::hide();
                    screen.activity();
                    swallow = true;
                }
                input::Event::TouchDown { x, y } => {
                    swallow = screen.activity();
                    if !swallow {
                        let (lx, ly) = gfx::panel_to_logical(ui.rotation, pw, ph, x, y);
                        ui.down(lx, ly);
                    }
                }
                input::Event::TouchMove { x, y } if !swallow => {
                    screen.activity();
                    let (lx, ly) = gfx::panel_to_logical(ui.rotation, pw, ph, x, y);
                    ui.moved(lx, ly);
                }
                input::Event::TouchUp { x, y } => {
                    if !swallow {
                        let (lx, ly) = gfx::panel_to_logical(ui.rotation, pw, ph, x, y);
                        ui.up(lx, ly);
                    }
                    swallow = false;
                }
                input::Event::TouchMove { .. } => {}
                input::Event::PointerRel { dx, dy, buttons } => {
                    let (w, h) = (ui.canvas.w, ui.canvas.h);
                    ui.pointer = (
                        (ui.pointer.0 + dx as i32 * 2).clamp(0, w - 1),
                        (ui.pointer.1 + dy as i32 * 2).clamp(0, h - 1),
                    );
                    ui.pointer_seen = time::now_ms();
                    let was_dark = screen.activity();
                    let (x, y) = ui.pointer;
                    let click = buttons & 1 != 0 && ui.buttons & 1 == 0;
                    match (buttons & 1 != 0, ui.buttons & 1 != 0) {
                        _ if menu.is_some() => {
                            if click {
                                menu = menu_tap(menu, ui.canvas.w, ui.canvas.h, x, y);
                            }
                        }
                        (true, false) if !was_dark => ui.down(x, y),
                        (true, true) => ui.moved(x, y),
                        (false, true) => ui.up(x, y),
                        _ => {}
                    }
                    ui.buttons = buttons;
                }
                input::Event::Key { .. } => {
                    screen.activity();
                }
                input::Event::Rotation(_) => {}
            }
        }
        // Released before it became a long press: screen off, or back on.
        if core::mem::take(&mut short_press) {
            if matches!(menu, Some(PowerMenu::Open { .. })) {
                menu = None;
            } else if screen.dark() {
                screen.activity();
            } else {
                info!("power button: screen off");
                screen.switch_off();
            }
            dirty = true;
        }
        // Held long enough: the power menu (the button may still be down).
        if power_down_at.is_some_and(|t| time::now_ms() - t >= LONG_PRESS_MS) {
            power_down_at = None;
            info!("power button: menu");
            menu = Some(PowerMenu::Open {
                since: time::now_ms(),
            });
            dirty = true;
        }
        match menu {
            Some(PowerMenu::Open { since }) if time::now_ms() - since > MENU_TIMEOUT_MS => {
                menu = None;
                dirty = true;
            }
            // Keep the screen on while the menu is up.
            Some(_) => {
                screen.activity();
            }
            None => {}
        }
        // Orientation from the accelerometer: rebuild for the new logical size.
        let rot = input::rotation();
        if rot != ui.rotation {
            ui.rotation = rot;
            let (w, h) = gfx::logical_size(rot, pw, ph);
            ui.canvas = Canvas::new(w, h);
            ui.pointer = (w / 2, h / 2);
            // Same rooms and tiles, new geometry: keep the selected room.
            let room = ui.room;
            let tiles = core::mem::take(&mut ui.tiles);
            let rooms = core::mem::take(&mut ui.rooms);
            ui.layout(tiles, rooms);
            ui.select_room(room);
            dirty = true;
        }
        let (version, connected, registry) = {
            let st = ha::STATE.lock();
            (st.version, st.connected, st.registry_version)
        };
        // (Re)build tiles once connected, and again when the room registries arrive.
        if connected && tiles_for_registry != Some(registry) {
            let (tiles, rooms) = tile_entities(&ha::STATE.lock());
            info!("{} tiles in {} rooms", tiles.len(), rooms.len());
            ui.layout(tiles, rooms);
            tiles_for_registry = Some(registry);
            dirty = true;
        }
        if version != drawn_version && ui.gesture == Gesture::None && ui.preview.take().is_some() {
            dirty = true; // the real state has arrived
        }
        let status = (connected, net::with(|n| n.ipv4.is_some()).unwrap_or(false));
        if status != drawn_status || ui.pointer_visible() != ui.pointer_drawn {
            dirty = true;
        }
        let minute = time::unix_now() / 60;
        let local = (time::unix_now() != 0)
            .then(|| time::local(time::unix_now(), ui.utc_offset, ui.eu_dst));
        screen.update(local.map(|t| t.hour * 60 + t.minute));
        if ui.pressed.is_some_and(|(_, at)| time::now_ms() - at > 400) {
            ui.pressed = None;
            dirty = true;
        }
        // Most of the house isn't on this page: a new HA snapshot that changes
        // nothing here needs no frame (on the Toon, drawing one costs 50 ms).
        if version != drawn_version
            && !dirty
            && minute == drawn_minute
            && ui.shown(&ha::STATE.lock()) == drawn_entities
        {
            drawn_version = version;
        }
        if dirty || version != drawn_version || minute != drawn_minute {
            let t0 = time::now_ms();
            {
                let st = ha::STATE.lock();
                ui.render(&st);
                drawn_entities = ui.shown(&st);
            }
            drawn_status = status;
            if let Some((pin, secs)) = &shown_pin {
                draw_pin(&mut ui.canvas, pin, *secs);
            }
            if let Some(m) = menu {
                draw_power_menu(&mut ui.canvas, m);
            }
            let t1 = time::now_ms();
            ui.canvas.present(&mut fb, ui.rotation);
            let t2 = time::now_ms();
            ::log::trace!("frame in {} ms (draw {}, present {})", t2 - t0, t1 - t0, t2 - t1);
            drawn_version = version;
            drawn_minute = minute;
            dirty = false;
            // "Switching off" is on screen: now do it, after a moment to
            // read it (and for the log to reach the network).
            if let Some(PowerMenu::Busy(action)) = menu {
                info!("power: {action:?} in 0.5 s");
                exec::sleep_ms(500).await;
                power(action);
                menu = None; // still here: it failed
                dirty = true;
            }
        }
        // While something on screen follows the clock (a press's highlight,
        // a held power button, a finger moving, the pointer), frames come
        // every 16 ms. Otherwise we sleep until an event (input, HA, the
        // network, the PIN), checking every second for what changes with
        // time (the minute, dimming, the PIN's countdown, settings).
        let moving = ui.pressed.is_some() || power_down_at.is_some() || ui.gesture != Gesture::None || ui.pointer_visible();
        if moving || dirty {
            exec::sleep_ms(16).await;
        } else {
            exec::wait_bits_or_ms(events::bit(), 1000).await;
        }
    }
}

