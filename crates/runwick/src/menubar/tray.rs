//! The macOS status item and its menu. AppKit needs a running application loop on the main
//! thread; winit (already used by the window) provides it with no window and no Dock icon.
//! A background thread looks at the host every few seconds and wakes the loop only when what
//! is shown changes, so the process sleeps otherwise.

use super::launch;
use super::model::{self, Exit, MAX_ROWS, Poll, Target, View, Watch};
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{AllocAnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, class, define_class, msg_send, sel};
use objc2_app_kit::{NSColor, NSFont, NSFontAttributeName, NSForegroundColorAttributeName};
use objc2_foundation::{NSMutableAttributedString, NSObject, NSRange, NSString};
use runwick_app::presence::{OpenRequest, window_open};
use runwick_core::api::activity::Group;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};
use winit::application::ApplicationHandler;
use winit::event::{StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::platform::macos::{ActivationPolicy, EventLoopBuilderExtMacOS};
use winit::window::WindowId;

const EVERY: Duration = Duration::from_secs(3);
/// After asking for the window, ignore repeated clicks while it starts (we leave once it is up).
const LAUNCH_GRACE: Duration = Duration::from_secs(10);
/// Template image (black + alpha): macOS draws it in the menu bar's own color.
const ICON_PNG: &[u8] = include_bytes!("../../../../assets/icons/tray@2x.png");

const ID_OPEN: &str = "open";
const ID_ACTIVITY: &str = "activity";
const ID_QUIT: &str = "quit";
const ID_HIDE: &str = "hide";
const ID_ROW: &str = "row:";

enum Event {
    Menu(MenuEvent),
    View(View),
    Exit(Exit),
    /// Runwick.app was opened again (Finder, Spotlight, `open`) while this process runs from it.
    Reopen,
}

pub fn run(exe: PathBuf, first: View) -> anyhow::Result<()> {
    let mut builder = EventLoop::<Event>::with_user_event();
    // An accessory: no Dock icon, no app menu, and never take focus from what the person is doing.
    builder
        .with_activation_policy(ActivationPolicy::Accessory)
        .with_default_menu(false)
        .with_activate_ignoring_other_apps(false);
    let event_loop = builder.build()?;
    event_loop.set_control_flow(ControlFlow::Wait);
    let proxy = event_loop.create_proxy();
    let menu_proxy = proxy.clone();
    MenuEvent::set_event_handler(Some(move |e| {
        let _ = menu_proxy.send_event(Event::Menu(e));
    }));
    let shown = first.clone();
    let watch_proxy = proxy.clone();
    std::thread::spawn(move || watch(watch_proxy, shown));
    let mut tray = Tray { exe, view: first, icon: None, launched: None, exit: None, proxy, reopen: None };
    event_loop.run_app(&mut tray)?;
    if let Some(e) = tray.exit {
        super::log(e.reason());
    }
    Ok(())
}

/// Look every few seconds; tell the loop only about changes and about leaving.
fn watch(proxy: EventLoopProxy<Event>, mut shown: View) {
    let mut watch = Watch::default();
    loop {
        std::thread::sleep(EVERY);
        let look = super::poll::look();
        if let (Poll::Failed(e), Some(_)) = (&look, &shown) {
            super::log(&format!("실행 상태를 확인하지 못했어요: {e}"));
        }
        let event = match watch.observe(look, window_open()) {
            Err(exit) => Event::Exit(exit),
            Ok(view) if model::looks_same(&view, &shown) => continue,
            Ok(view) => {
                shown = view.clone();
                Event::View(view)
            }
        };
        let leaving = matches!(event, Event::Exit(_));
        if proxy.send_event(event).is_err() || leaving {
            return;
        }
    }
}

struct Tray {
    exe: PathBuf,
    view: View,
    icon: Option<TrayIcon>,
    launched: Option<Instant>,
    exit: Option<Exit>,
    proxy: EventLoopProxy<Event>,
    /// Kept alive while it is registered for the reopen event.
    reopen: Option<Retained<ReopenHandler>>,
}

impl ApplicationHandler<Event> for Tray {
    fn new_events(&mut self, event_loop: &ActiveEventLoop, cause: StartCause) {
        // The status item can only be made once the application loop runs.
        if cause == StartCause::Init {
            // After AppKit installed its own handlers, so this one replaces its reopen handler.
            self.reopen = MainThreadMarker::new().map(|mtm| ReopenHandler::install(mtm, self.proxy.clone()));
            match build_icon(&self.view) {
                Ok(icon) => {
                    self.icon = Some(icon);
                    self.refresh();
                }
                Err(e) => {
                    super::log(&format!("메뉴 막대 아이콘을 만들지 못했어요: {e}"));
                    event_loop.exit();
                }
            }
        }
    }

    fn resumed(&mut self, _event_loop: &ActiveEventLoop) {}

    fn window_event(&mut self, _event_loop: &ActiveEventLoop, _id: WindowId, _event: WindowEvent) {}

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: Event) {
        match event {
            Event::View(view) => {
                self.view = view;
                self.refresh();
            }
            Event::Exit(exit) => {
                self.exit = Some(exit);
                event_loop.exit();
            }
            Event::Menu(e) => self.on_menu(event_loop, e.id.as_ref()),
            Event::Reopen => self.on_menu(event_loop, ID_OPEN),
        }
    }
}

impl Tray {
    fn on_menu(&mut self, event_loop: &ActiveEventLoop, id: &str) {
        let req = match id {
            ID_HIDE => {
                super::log("메뉴 막대 아이콘 끄기를 골랐어요");
                event_loop.exit();
                return;
            }
            ID_OPEN => None,
            ID_ACTIVITY => Some(OpenRequest::Activity),
            ID_QUIT => Some(OpenRequest::Quit),
            other => match other.strip_prefix(ID_ROW).and_then(|s| s.parse().ok()) {
                Some(session) => Some(OpenRequest::Session(session)),
                None => return,
            },
        };
        if self.launched.is_some_and(|t| t.elapsed() < LAUNCH_GRACE) {
            return;
        }
        match launch::open_window(&self.exe, req) {
            Ok(()) => self.launched = Some(Instant::now()),
            Err(e) => super::log(&format!("창을 열지 못했어요: {e}")),
        }
    }

    fn refresh(&self) {
        let Some(icon) = &self.icon else { return };
        icon.set_menu(Some(Box::new(menu(&self.view))));
        let _ = icon.set_tooltip(Some(model::tooltip(&self.view)));
        icon.set_title(model::title(&self.view));
        tint_needs(icon, model::needs_span(&self.view));
    }
}

fn build_icon(view: &View) -> anyhow::Result<TrayIcon> {
    let img = image::load_from_memory(ICON_PNG)?.into_rgba8();
    let (w, h) = img.dimensions();
    let icon = Icon::from_rgba(img.into_raw(), w, h)?;
    let mut b = TrayIconBuilder::new().with_icon_templated(icon).with_tooltip(model::tooltip(view));
    if let Some(t) = model::title(view) {
        b = b.with_title(t);
    }
    Ok(b.build()?)
}

fn menu(view: &View) -> Menu {
    let menu = Menu::new();
    let note = |text: &str| MenuItem::new(text, false, None);
    let item = |id: &str, text: &str| MenuItem::with_id(id, text, true, None);
    let sep = PredefinedMenuItem::separator;
    let mut items: Vec<Box<dyn tray_icon::menu::IsMenuItem>> = vec![
        Box::new(note("창은 닫혔고, 작업은 계속되고 있어요")),
        Box::new(note("다시 열면 같은 실행에 그대로 연결됩니다.")),
        Box::new(sep()),
        Box::new(note(&model::status_line(view))),
    ];
    if let Some(s) = view {
        for row in s.rows.iter().take(MAX_ROWS) {
            let id = match row.target {
                Target::Session(session) => format!("{ID_ROW}{session}"),
                Target::Activity => ID_ACTIVITY.to_string(),
            };
            let it = item(&id, &model::row_text(row));
            style_row(&it, row);
            items.push(Box::new(it));
        }
        if s.rows.len() > MAX_ROWS {
            items.push(Box::new(item(ID_ACTIVITY, &model::more_label(s.rows.len() - MAX_ROWS))));
        }
    }
    items.push(Box::new(sep()));
    items.push(Box::new(item(ID_OPEN, "Runwick 열기")));
    items.push(Box::new(item(ID_ACTIVITY, "전체 활동")));
    items.push(Box::new(item(ID_QUIT, "작업까지 종료…")));
    items.push(Box::new(sep()));
    items.push(Box::new(item(ID_HIDE, "메뉴 막대 아이콘 끄기")));
    let refs: Vec<&dyn tray_icon::menu::IsMenuItem> = items.iter().map(|b| b.as_ref()).collect();
    if let Err(e) = menu.append_items(&refs) {
        super::log(&format!("메뉴를 만들지 못했어요: {e}"));
    }
    menu
}

/// A row on two lines, as in the design: the title in the menu's text color, then its state in
/// words and color (the ember-equivalent orange when it needs the person, green while it runs,
/// secondary otherwise); below it, smaller and dimmed, the project / workspace and the tool.
fn style_row(item: &MenuItem, row: &model::Row) {
    let label = model::row_label(row);
    let state_at = utf16_len(&model::short_title(&row.title)) + utf16_len(model::ROW_SEPARATOR);
    let text = attributed(&model::row_text(row), &NSFont::menuFontOfSize(0.0));
    let color = match row.group {
        g if model::needs_person(g) => NSColor::systemOrangeColor(),
        Some(Group::Running) => NSColor::systemGreenColor(),
        _ => NSColor::secondaryLabelColor(),
    };
    color_range(&text, &color, state_at, utf16_len(&row.state));
    let meta_at = utf16_len(&label) + 1;
    let meta_len = text.length().saturating_sub(meta_at);
    font_range(&text, &NSFont::menuFontOfSize(11.0), meta_at, meta_len);
    color_range(&text, &NSColor::secondaryLabelColor(), meta_at, meta_len);
    item.set_attributed_title(Some(&text));
}

/// Tint the 확인할 일 count at the start of the status item's title.
fn tint_needs(icon: &TrayIcon, span: usize) {
    if span == 0 {
        return;
    }
    let (Some(item), Some(mtm)) = (icon.ns_status_item(), MainThreadMarker::new()) else {
        return;
    };
    let Some(button) = item.button(mtm) else {
        return;
    };
    let current = button.attributedTitle();
    let text = NSMutableAttributedString::initWithAttributedString(NSMutableAttributedString::alloc(), &current);
    color_range(&text, &NSColor::systemOrangeColor(), 0, span);
    button.setAttributedTitle(&text);
}

fn attributed(s: &str, font: &NSFont) -> Retained<NSMutableAttributedString> {
    let text = NSMutableAttributedString::initWithString(NSMutableAttributedString::alloc(), &NSString::from_str(s));
    // SAFETY: NSFontAttributeName takes an NSFont; the range is the whole string.
    unsafe { text.addAttribute_value_range(NSFontAttributeName, font, NSRange::new(0, utf16_len(s))) };
    text
}

fn font_range(text: &NSMutableAttributedString, font: &NSFont, start: usize, len: usize) {
    if len == 0 || start + len > text.length() {
        return;
    }
    // SAFETY: NSFontAttributeName takes an NSFont; the range was checked above.
    unsafe { text.addAttribute_value_range(NSFontAttributeName, font, NSRange::new(start, len)) };
}

fn color_range(text: &NSMutableAttributedString, color: &NSColor, start: usize, len: usize) {
    if len == 0 || start + len > text.length() {
        return;
    }
    // SAFETY: NSForegroundColorAttributeName takes an NSColor; the range was checked above.
    unsafe { text.addAttribute_value_range(NSForegroundColorAttributeName, color, NSRange::new(start, len)) };
}

fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

/// Launch Services counts this process as the running Runwick when it runs from the app bundle,
/// so opening Runwick.app again only sends it a "reopen" Apple Event. Answer it by opening the
/// window, as the person asked.
struct ReopenIvars {
    proxy: EventLoopProxy<Event>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "RunwickMenuBarReopenHandler"]
    #[ivars = ReopenIvars]
    struct ReopenHandler;

    impl ReopenHandler {
        #[unsafe(method(handleReopen:withReplyEvent:))]
        fn handle_reopen(&self, _event: &AnyObject, _reply: &AnyObject) {
            let _ = self.ivars().proxy.send_event(Event::Reopen);
        }
    }
);

impl ReopenHandler {
    fn install(mtm: MainThreadMarker, proxy: EventLoopProxy<Event>) -> Retained<Self> {
        /// `kCoreEventClass` and `kAEReopenApplication` (FourCharCodes).
        const CORE_EVENT_CLASS: u32 = u32::from_be_bytes(*b"aevt");
        const REOPEN_APPLICATION: u32 = u32::from_be_bytes(*b"rapp");
        let this = Self::alloc(mtm).set_ivars(ReopenIvars { proxy });
        // SAFETY: plain NSObject initialisation of our own subclass.
        let this: Retained<Self> = unsafe { msg_send![super(this), init] };
        // SAFETY: the shared manager exists in every AppKit process; the handler object outlives
        // the registration (kept in `Tray`) and answers the selector with the expected arguments.
        unsafe {
            let manager: Retained<NSObject> = msg_send![class!(NSAppleEventManager), sharedAppleEventManager];
            let _: () = msg_send![
                &manager,
                setEventHandler: &*this,
                andSelector: sel!(handleReopen:withReplyEvent:),
                forEventClass: CORE_EVENT_CLASS,
                andEventID: REOPEN_APPLICATION
            ];
        }
        this
    }
}
