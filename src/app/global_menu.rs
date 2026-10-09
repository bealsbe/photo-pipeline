//! The KDE Plasma global menu.
//!
//! The menu bar is exported on the session bus as `com.canonical.dbusmenu`,
//! and the main window is linked to it: on Wayland through KWin's
//! `org_kde_kwin_appmenu` protocol, on X11 through the AppMenu registrar.
//! While the registrar is present (a Global Menu widget is in the panel) the
//! in-window menu bar is hidden, as Qt apps do; take the widget away and the
//! menu bar comes back.
//!
//! The exported menu is built from the same command table as the in-window
//! menus, so labels, check marks, enabled state and shortcuts always agree.

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{OwnedValue, StructureBuilder, Value};

use super::commands::{Cmd, MENUS};
use crate::bus::{BgEvent, Bus};

pub const OBJECT_PATH: &str = "/MenuBar/1";
const REGISTRAR: &str = "com.canonical.AppMenu.Registrar";

/// One entry of an exported menu.
#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub cmd: Cmd,
    pub enabled: bool,
    /// `Some` for checkable entries.
    pub checked: Option<bool>,
}

/// The whole menu bar: per top-level menu, its entries (`None` = separator).
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Model {
    pub menus: Vec<Vec<Option<Item>>>,
}

struct Shared {
    model: Model,
    revision: u32,
}

pub struct GlobalMenu {
    conn: zbus::blocking::Connection,
    shared: Arc<Mutex<Shared>>,
}

impl GlobalMenu {
    /// Connect to the session bus and serve the menu.  `None` when it's
    /// turned off (Preferences, or `PP_NO_GLOBAL_MENU`) or there is no
    /// session bus: then the in-window menu bar simply stays.
    pub fn start(bus: Bus, enabled: bool) -> Option<GlobalMenu> {
        if !enabled {
            return None;
        }
        let shared = Arc::new(Mutex::new(Shared { model: Model::default(), revision: 1 }));
        let iface = DbusMenu { shared: shared.clone(), bus: bus.clone() };
        let conn = zbus::blocking::connection::Builder::session().ok()?.serve_at(OBJECT_PATH, iface).ok()?.build().ok()?;
        watch_registrar(conn.clone(), bus);
        Some(GlobalMenu { conn, shared })
    }

    /// The bus name the window is linked to.
    pub fn service(&self) -> String {
        self.conn.unique_name().map(|n| n.to_string()).unwrap_or_default()
    }

    pub fn connection(&self) -> zbus::blocking::Connection {
        self.conn.clone()
    }

    /// Publish a new state of the menus (cheap when nothing changed).
    pub fn update(&self, model: Model) {
        let revision = {
            let mut s = self.shared.lock();
            if s.model == model {
                return;
            }
            s.model = model;
            s.revision += 1;
            s.revision
        };
        let conn = self.conn.clone();
        // Signals go out on the bus's own executor; don't hold up the UI.
        std::thread::spawn(move || {
            if let Ok(emitter) = SignalEmitter::new(conn.inner(), OBJECT_PATH) {
                let _ = zbus::block_on(DbusMenu::layout_updated(&emitter, revision, 0));
            }
        });
    }
}

/// Tell the app whether a global menu is on screen now, and whenever that
/// changes.
fn watch_registrar(conn: zbus::blocking::Connection, bus: Bus) {
    std::thread::Builder::new()
        .name("global-menu".into())
        .spawn(move || {
            let Ok(dbus) = zbus::blocking::fdo::DBusProxy::new(&conn) else { return };
            let present = zbus::names::BusName::try_from(REGISTRAR).ok().and_then(|n| dbus.name_has_owner(n).ok()).unwrap_or(false);
            let _ = bus.unbounded_send(BgEvent::GlobalMenu(present));
            let Ok(changes) = dbus.receive_name_owner_changed_with_args(&[(0, REGISTRAR)]) else { return };
            for signal in changes {
                if let Ok(args) = signal.args() {
                    let _ = bus.unbounded_send(BgEvent::GlobalMenu(args.new_owner().is_some()));
                }
            }
        })
        .ok();
}

/// Link the window to the exported menu.  Runs on the window's thread.
pub fn register_window(window: &dyn iced::window::Window, service: String, conn: zbus::blocking::Connection) -> Result<(), String> {
    use iced::window::raw_window_handle::{RawDisplayHandle, RawWindowHandle};
    let wh = window.window_handle().map_err(|e| e.to_string())?.as_raw();
    let dh = window.display_handle().map_err(|e| e.to_string())?.as_raw();
    match (wh, dh) {
        (RawWindowHandle::Wayland(w), RawDisplayHandle::Wayland(d)) => wayland::set_address(d.display.as_ptr(), w.surface.as_ptr(), &service),
        (RawWindowHandle::Xlib(w), _) => register_x11(&conn, w.window as u32),
        (RawWindowHandle::Xcb(w), _) => register_x11(&conn, w.window.get()),
        _ => Err("unsupported window system".into()),
    }
}

fn register_x11(conn: &zbus::blocking::Connection, window: u32) -> Result<(), String> {
    let path = zbus::zvariant::ObjectPath::try_from(OBJECT_PATH).map_err(|e| e.to_string())?;
    conn.call_method(Some(REGISTRAR), "/com/canonical/AppMenu/Registrar", Some(REGISTRAR), "RegisterWindow", &(window, path))
        .map(|_| ())
        .map_err(|e| e.to_string())
}

mod wayland {
    use std::ffi::c_void;

    use wayland_client::backend::{Backend, ObjectId};
    use wayland_client::globals::{GlobalListContents, registry_queue_init};
    use wayland_client::protocol::{wl_registry, wl_surface::WlSurface};
    use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
    use wayland_protocols_plasma::appmenu::client::{org_kde_kwin_appmenu::OrgKdeKwinAppmenu, org_kde_kwin_appmenu_manager::OrgKdeKwinAppmenuManager};

    struct State;

    impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
        fn event(_: &mut Self, _: &wl_registry::WlRegistry, _: wl_registry::Event, _: &GlobalListContents, _: &Connection, _: &QueueHandle<Self>) {}
    }
    impl Dispatch<OrgKdeKwinAppmenuManager, ()> for State {
        fn event(_: &mut Self, _: &OrgKdeKwinAppmenuManager, _: <OrgKdeKwinAppmenuManager as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
    }
    impl Dispatch<OrgKdeKwinAppmenu, ()> for State {
        fn event(_: &mut Self, _: &OrgKdeKwinAppmenu, _: <OrgKdeKwinAppmenu as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
    }

    /// Attach the dbusmenu address to the window's surface through KWin's
    /// appmenu protocol, on the display the window already uses.
    pub fn set_address(display: *mut c_void, surface: *mut c_void, service: &str) -> Result<(), String> {
        // SAFETY: the pointers come from the live window's raw handles; the
        // foreign display is not closed when our Connection is dropped.
        let backend = unsafe { Backend::from_foreign_display(display.cast()) };
        let conn = Connection::from_backend(backend);
        let (globals, mut queue) = registry_queue_init::<State>(&conn).map_err(|e| e.to_string())?;
        let qh = queue.handle();
        let manager: OrgKdeKwinAppmenuManager = globals.bind(&qh, 1..=2, ()).map_err(|_| "the compositor has no appmenu support".to_string())?;
        // SAFETY: `surface` is the window's live wl_surface.
        let id = unsafe { ObjectId::from_ptr(WlSurface::interface(), surface.cast()) }.map_err(|e| e.to_string())?;
        let surface = WlSurface::from_id(&conn, id).map_err(|e| e.to_string())?;
        let appmenu = manager.create(&surface, &qh, ());
        appmenu.set_address(service.to_string(), super::OBJECT_PATH.to_string());
        queue.roundtrip(&mut State).map_err(|e| e.to_string())?;
        // The link lasts as long as the window; keep the objects alive.
        std::mem::forget((appmenu, manager, queue, conn));
        Ok(())
    }
}

// ── com.canonical.dbusmenu ──────────────────────────────────────────────────

struct DbusMenu {
    shared: Arc<Mutex<Shared>>,
    bus: Bus,
}

/// Ids: 0 the root, `m + 1` a top-level menu, `(m + 1) * 100 + i` its
/// entries (separators included, so ids stay put).
fn menu_id(m: usize) -> i32 {
    m as i32 + 1
}

fn item_id(m: usize, i: usize) -> i32 {
    (m as i32 + 1) * 100 + i as i32
}

fn ov<'a>(v: impl Into<Value<'a>>) -> OwnedValue {
    OwnedValue::try_from(v.into()).expect("plain values carry no file descriptors")
}

/// "Ctrl+Shift+E" → [["Control", "Shift", "E"]] (dbusmenu's key naming).
fn shortcut(cmd: Cmd) -> Option<Vec<Vec<String>>> {
    let s = match cmd {
        Cmd::PruneSelected => "P",
        Cmd::MarkSelected => "M",
        c => c.shortcut(),
    };
    if s.is_empty() {
        return None;
    }
    let keys = s
        .split('+')
        .filter(|k| !k.is_empty())
        .map(|k| match k {
            "Ctrl" => "Control".to_string(),
            "\\" => "backslash".to_string(),
            "?" => "question".to_string(),
            "," => "comma".to_string(),
            k => k.to_string(),
        })
        .collect();
    Some(vec![keys])
}

/// A KDE icon for each command, so the global menu shows the same icons as
/// the rest of the desktop.
fn icon_name(cmd: Cmd) -> &'static str {
    match cmd {
        Cmd::Open => "document-open-folder",
        Cmd::Export => "document-export",
        Cmd::EditCopy => "edit-copy",
        Cmd::Review => "user-trash",
        Cmd::Sort => "view-sort",
        Cmd::GridView => "view-list-icons",
        Cmd::CalendarView => "view-calendar",
        Cmd::Search => "system-search",
        Cmd::Subjects => "system-users",
        Cmd::SelectMode => "edit-select",
        Cmd::QuickLook => "view-preview",
        Cmd::PruneSelected => "dialog-cancel",
        Cmd::MarkSelected => "dialog-ok-apply",
        Cmd::Undo => "edit-undo",
        Cmd::Redo => "edit-redo",
        Cmd::Quit => "application-exit",
        Cmd::Shortcuts => "configure-shortcuts",
        Cmd::SideMenu => "sidebar-show",
        Cmd::FolderBar => "folder",
        Cmd::ToReview => "view-filter",
        Cmd::FileNames => "insert-text",
        Cmd::Compare => "view-split-left-right",
        Cmd::ReviewBurst => "object-group",
        Cmd::StackBurst => "object-group",
        Cmd::StackBursts => "view-group",
        Cmd::AdvanceAfterMark => "media-skip-forward",
        Cmd::Preferences => "configure",
        Cmd::Analyse => "edit-find",
        Cmd::ProblemsOnly => "view-filter",
        Cmd::KeepThisPruneRest => "dialog-ok-apply",
        Cmd::PruneSoft | Cmd::PruneAccidental | Cmd::PruneCopies => "dialog-cancel",
        Cmd::PrevBurst => "go-previous",
        Cmd::NextBurst => "go-next",
    }
}

fn props_for(model: &Model, id: i32) -> Option<HashMap<String, OwnedValue>> {
    let mut p: HashMap<String, OwnedValue> = HashMap::new();
    if id == 0 {
        p.insert("children-display".into(), ov("submenu"));
        return Some(p);
    }
    if id < 100 {
        let m = (id - 1) as usize;
        let (title, mnemonic, _) = MENUS.get(m)?;
        // "_" marks the mnemonic letter.
        let label = match title.char_indices().find(|(_, c)| c.to_ascii_lowercase() == *mnemonic) {
            Some((i, _)) => format!("{}_{}", &title[..i], &title[i..]),
            None => title.to_string(),
        };
        p.insert("label".into(), ov(label));
        p.insert("children-display".into(), ov("submenu"));
        return Some(p);
    }
    let (m, i) = ((id / 100 - 1) as usize, (id % 100) as usize);
    match model.menus.get(m)?.get(i)? {
        None => {
            p.insert("type".into(), ov("separator"));
        }
        Some(item) => {
            let label = item.cmd.label().replace('_', "__");
            p.insert("label".into(), ov(label));
            p.insert("icon-name".into(), ov(icon_name(item.cmd)));
            if !item.enabled {
                p.insert("enabled".into(), ov(false));
            }
            if let Some(checked) = item.checked {
                p.insert("toggle-type".into(), ov("checkmark"));
                p.insert("toggle-state".into(), ov(if checked { 1i32 } else { 0i32 }));
            }
            if let Some(keys) = shortcut(item.cmd) {
                p.insert("shortcut".into(), ov(keys));
            }
        }
    }
    Some(p)
}

fn children_of(model: &Model, id: i32) -> Vec<i32> {
    if id == 0 {
        return (0..MENUS.len()).map(menu_id).collect();
    }
    if id < 100 {
        let m = (id - 1) as usize;
        return model.menus.get(m).map(|items| (0..items.len()).map(|i| item_id(m, i)).collect()).unwrap_or_default();
    }
    Vec::new()
}

type Layout = (i32, HashMap<String, OwnedValue>, Vec<OwnedValue>);

fn layout(model: &Model, id: i32, depth: i32) -> Layout {
    let props = props_for(model, id).unwrap_or_default();
    let children = if depth == 0 {
        Vec::new()
    } else {
        children_of(model, id)
            .into_iter()
            .map(|c| {
                let (cid, cprops, cchildren) = layout(model, c, depth - 1);
                let s = StructureBuilder::new().add_field(cid).add_field(cprops).add_field(cchildren).build().expect("layout structure");
                ov(Value::Structure(s))
            })
            .collect()
    };
    (id, props, children)
}

fn filtered(mut props: HashMap<String, OwnedValue>, names: &[String]) -> HashMap<String, OwnedValue> {
    if !names.is_empty() {
        props.retain(|k, _| names.contains(k));
    }
    props
}

#[zbus::interface(name = "com.canonical.dbusmenu")]
impl DbusMenu {
    #[zbus(property)]
    fn version(&self) -> u32 {
        3
    }

    #[zbus(property)]
    fn text_direction(&self) -> String {
        "ltr".into()
    }

    #[zbus(property)]
    fn status(&self) -> String {
        "normal".into()
    }

    #[zbus(property)]
    fn icon_theme_path(&self) -> Vec<String> {
        Vec::new()
    }

    fn get_layout(&self, parent_id: i32, recursion_depth: i32, property_names: Vec<String>) -> (u32, Layout) {
        let s = self.shared.lock();
        let (id, props, children) = layout(&s.model, parent_id, recursion_depth);
        (s.revision, (id, filtered(props, &property_names), children))
    }

    fn get_group_properties(&self, ids: Vec<i32>, property_names: Vec<String>) -> Vec<(i32, HashMap<String, OwnedValue>)> {
        let s = self.shared.lock();
        ids.into_iter().filter_map(|id| props_for(&s.model, id).map(|p| (id, filtered(p, &property_names)))).collect()
    }

    fn get_property(&self, id: i32, name: String) -> zbus::fdo::Result<OwnedValue> {
        let s = self.shared.lock();
        props_for(&s.model, id)
            .and_then(|mut p| p.remove(&name))
            .ok_or_else(|| zbus::fdo::Error::InvalidArgs(format!("no property {name} on item {id}")))
    }

    fn event(&self, id: i32, event_id: String, _data: OwnedValue, _timestamp: u32) {
        if event_id != "clicked" || id < 100 {
            return;
        }
        let s = self.shared.lock();
        let (m, i) = ((id / 100 - 1) as usize, (id % 100) as usize);
        if let Some(Some(item)) = s.model.menus.get(m).and_then(|items| items.get(i)) {
            if item.enabled {
                let _ = self.bus.unbounded_send(BgEvent::MenuCommand(item.cmd));
            }
        }
    }

    fn event_group(&self, events: Vec<(i32, String, OwnedValue, u32)>) -> Vec<i32> {
        for (id, event_id, data, ts) in events {
            self.event(id, event_id, data, ts);
        }
        Vec::new()
    }

    fn about_to_show(&self, _id: i32) -> bool {
        false
    }

    fn about_to_show_group(&self, _ids: Vec<i32>) -> (Vec<i32>, Vec<i32>) {
        (Vec::new(), Vec::new())
    }

    #[zbus(signal)]
    async fn layout_updated(emitter: &SignalEmitter<'_>, revision: u32, parent: i32) -> zbus::Result<()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortcuts_use_dbusmenu_names() {
        assert_eq!(shortcut(Cmd::EditCopy), Some(vec![vec!["Control".to_string(), "Shift".into(), "E".into()]]));
        assert_eq!(shortcut(Cmd::SideMenu), Some(vec![vec!["Control".to_string(), "backslash".into()]]));
        assert_eq!(shortcut(Cmd::PruneSelected), Some(vec![vec!["P".to_string()]]));
        assert_eq!(shortcut(Cmd::Preferences), Some(vec![vec!["Control".to_string(), "comma".into()]]));
        assert_eq!(shortcut(Cmd::Search), Some(vec![vec!["Control".to_string(), "F".into()]]));
        assert_eq!(shortcut(Cmd::Subjects), Some(vec![vec!["Control".to_string(), "4".into()]]));
    }

    #[test]
    fn layout_nests_menus_and_marks_mnemonics() {
        let model = Model {
            menus: MENUS.iter().map(|(_, _, items)| items.iter().map(|c| c.map(|cmd| Item { cmd, enabled: true, checked: None })).collect()).collect(),
        };
        let (id, props, children) = layout(&model, 0, -1);
        assert_eq!(id, 0);
        assert!(props.contains_key("children-display"));
        assert_eq!(children.len(), MENUS.len());
        let file = props_for(&model, 1).unwrap();
        assert_eq!(String::try_from(file["label"].try_clone().unwrap()).unwrap(), "_File");
        let sep = props_for(&model, item_id(0, 1)).unwrap();
        assert_eq!(String::try_from(sep["type"].try_clone().unwrap()).unwrap(), "separator");
    }
}

#[cfg(test)]
mod bus_tests {
    use super::*;

    /// Turned off (Preferences or PP_NO_GLOBAL_MENU): nothing is served, and
    /// the session bus isn't even opened.
    #[test]
    fn off_serves_nothing() {
        let (bus, _rx) = crate::bus::channel();
        assert!(GlobalMenu::start(bus, false).is_none());
    }

    /// Serves the menu on the real session bus and reads it back the way
    /// Plasma does.  Ignored by default (needs a session bus).
    #[test]
    #[ignore]
    fn get_layout_over_the_bus() {
        let (bus, _rx) = crate::bus::channel();
        let gm = GlobalMenu::start(bus, true).expect("session bus");
        let model = Model {
            menus: MENUS.iter().map(|(_, _, items)| items.iter().map(|c| c.map(|cmd| Item { cmd, enabled: true, checked: Some(false) })).collect()).collect(),
        };
        gm.update(model);
        let client = zbus::blocking::Connection::session().unwrap();
        let reply = client.call_method(Some(gm.service().as_str()), OBJECT_PATH, Some("com.canonical.dbusmenu"), "GetLayout", &(0i32, -1i32, Vec::<String>::new()));
        match reply {
            Ok(msg) => {
                let body = msg.body();
                println!("signature: {:?}", body.signature());
                let (rev, (id, _props, children)): (u32, Layout) = body.deserialize().unwrap();
                println!("revision {rev}, root {id}, {} menus", children.len());
            }
            Err(e) => panic!("GetLayout failed: {e}"),
        }
    }
}
