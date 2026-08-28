//! Settings window. Layout follows the C# `SettingsWindow` / `SettingsCard`
//! pages: a 168px navigation pane with a 48px header, and pages built from a
//! title, group headers and cards.
//!
//! Only the pages whose settings this build can actually honour are here. The
//! format page arrives with AVIF/JXL encoding; the UI-language dropdown arrives
//! with i18n.

mod system;
pub mod text_input;
pub mod widgets;

use std::cell::{Cell, RefCell};
use std::time::Duration;

use gpui::{
    div, prelude::*, px, size, App, Bounds, Context, Entity, Focusable, FontWeight, MouseButton,
    SharedString, Subscription, Timer, TitlebarOptions, Window, WindowBounds, WindowHandle,
    WindowOptions,
};

use crate::config::Settings;
use crate::hotkey::{self, Hotkey, HotkeyAction, HotkeyBindings};
use crate::overlay::{theme, toolbar};
use text_input::TextInput;
use widgets::{CONTENT_MAX_W, TOOLBAR_COLUMNS};

thread_local! {
    static SETTINGS_WINDOW: RefCell<Option<WindowHandle<SettingsView>>> = const { RefCell::new(None) };
    static KEYS_BOUND: Cell<bool> = const { Cell::new(false) };
}

/// Long enough to press any combo, short enough that a recorder left running by
/// a closed window cannot keep swallowing keys. See `hook::record_proc`.
const RECORDING_TIMEOUT: Duration = Duration::from_secs(10);

/// Short values sit beside their label rather than stretching the card.
const INLINE_FIELD_W: f32 = 220.;

const LANGUAGES: [(&str, &str); 9] = [
    ("zh-CN", "简体中文"),
    ("zh-TW", "繁體中文"),
    ("en", "English"),
    ("ja", "日本語"),
    ("ko", "한국어"),
    ("fr", "Français"),
    ("de", "Deutsch"),
    ("es", "Español"),
    ("ru", "Русский"),
];

const HOTKEY_ROWS: [(HotkeyAction, &str, &str, &str, &str); 3] = [
    (
        HotkeyAction::Region,
        "hk-region",
        "settings/screenshot.svg",
        "区域截图",
        "冻结屏幕后框选区域，进入标注工具栏。",
    ),
    (
        HotkeyAction::CopyOnly,
        "hk-copy",
        "settings/copy.svg",
        "仅复制",
        "框选后直接复制到剪贴板，不写文件。",
    ),
    (
        HotkeyAction::Fullscreen,
        "hk-full",
        "settings/monitor.svg",
        "全屏截图",
        "抓取当前显示器并保存 PNG。",
    ),
];

/// Opens the window, or raises the one already open. Called from the tray.
pub fn open(cx: &mut App) {
    bind_keys(cx);

    if let Some(handle) = SETTINGS_WINDOW.with(|slot| *slot.borrow()) {
        let raised = handle
            .update(cx, |_, window, _| window.activate_window())
            .is_ok();
        if raised {
            return;
        }
        // The user closed it; the handle is stale.
        SETTINGS_WINDOW.with(|slot| *slot.borrow_mut() = None);
    }

    let bounds = Bounds::centered(None, size(px(920.), px(660.)), cx);
    let opened = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions {
                title: Some("Zshot".into()),
                ..Default::default()
            }),
            focus: true,
            show: true,
            window_min_size: Some(size(px(720.), px(480.))),
            ..Default::default()
        },
        |window, cx| cx.new(|cx| SettingsView::new(window, cx)),
    );

    match opened {
        Ok(handle) => SETTINGS_WINDOW.with(|slot| *slot.borrow_mut() = Some(handle)),
        Err(e) => eprintln!("settings window: {e}"),
    }
}

/// Routed from the hidden window's message loop; the recording hook posts raw
/// virtual keys because gpui never reports PrtSc as a keystroke.
pub fn on_hotkey_recorded(cx: &mut App, hotkey: Hotkey) {
    let Some(handle) = SETTINGS_WINDOW.with(|slot| *slot.borrow()) else {
        // No window to receive it — do not leave the recorder swallowing keys.
        crate::platform::stop_recording();
        return;
    };
    if handle
        .update(cx, |view, _, cx| view.hotkey_recorded(hotkey, cx))
        .is_err()
    {
        crate::platform::stop_recording();
    }
}

fn bind_keys(cx: &mut App) {
    if KEYS_BOUND.with(|b| b.get()) {
        return;
    }
    cx.bind_keys(text_input::key_bindings());
    KEYS_BOUND.with(|b| b.set(true));
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Screenshot,
    Ocr,
    Hotkey,
    General,
}

impl Page {
    const ALL: [(Page, &'static str, &'static str, &'static str); 4] = [
        (
            Page::Screenshot,
            "nav-screenshot",
            "settings/screenshot.svg",
            "截图",
        ),
        (Page::Ocr, "nav-ocr", "settings/ocr.svg", "文字识别"),
        (Page::Hotkey, "nav-hotkey", "settings/hotkey.svg", "快捷键"),
        (Page::General, "nav-general", "settings/general.svg", "通用"),
    ];

    fn title(self) -> &'static str {
        match self {
            Page::Screenshot => "截图",
            Page::Ocr => "文字识别",
            Page::Hotkey => "快捷键",
            Page::General => "通用",
        }
    }
}

pub struct SettingsView {
    page: Page,
    settings: Settings,
    engine: SharedString,
    status: SharedString,
    status_error: bool,
    language_open: bool,
    prompt_open: bool,
    /// Which action is being bound, if any. The recording hook is live for
    /// exactly as long as this is `Some`.
    recording: Option<HotkeyAction>,
    autostart: bool,
    /// What is currently in the credential vault, so blurring the field without
    /// touching it does not rewrite the entry on every focus change.
    saved_api_key: String,
    base_url: Entity<TextInput>,
    api_key: Entity<TextInput>,
    model: Entity<TextInput>,
    timeout: Entity<TextInput>,
    prompt: Entity<TextInput>,
    _subscriptions: Vec<Subscription>,
}

impl SettingsView {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let settings = crate::config::get();
        let t = settings.translation.clone();
        let saved_api_key = crate::translate::api_key().unwrap_or_default();

        let base_url = cx.new(|cx| {
            TextInput::new(cx, t.base_url.clone(), "https://api.openai.com/v1", false)
        });
        let api_key = cx.new(|cx| TextInput::new(cx, saved_api_key.clone(), "sk-...", true));
        let model = cx.new(|cx| TextInput::new(cx, t.model.clone(), "gpt-4o-mini", false));
        let timeout =
            cx.new(|cx| TextInput::new(cx, t.timeout_seconds.to_string(), "30", false));
        let prompt = cx.new(|cx| TextInput::new(cx, t.prompt.clone(), "使用内置提示词", false));

        let mut subscriptions = Vec::new();

        // Every field compares before storing: the inputs notify on cursor moves
        // too, and writing the file on a click would be pure churn.
        subscriptions.push(cx.observe(&base_url, |this, input, cx| {
            let value = input.read(cx).text().to_string();
            if this.settings.translation.base_url != value {
                this.settings.translation.base_url = value;
                this.persist(cx);
            }
        }));
        subscriptions.push(cx.observe(&model, |this, input, cx| {
            let value = input.read(cx).text().to_string();
            if this.settings.translation.model != value {
                this.settings.translation.model = value;
                this.persist(cx);
            }
        }));
        subscriptions.push(cx.observe(&prompt, |this, input, cx| {
            let value = input.read(cx).text().to_string();
            if this.settings.translation.prompt != value {
                this.settings.translation.prompt = value;
                this.persist(cx);
            }
        }));
        // Unparsable input keeps the last good value rather than resetting to a
        // default the user never chose; the field itself keeps what they typed.
        subscriptions.push(cx.observe(&timeout, |this, input, cx| {
            let Ok(parsed) = input.read(cx).text().trim().parse::<u32>() else {
                return;
            };
            let clamped = parsed.clamp(5, 300);
            if this.settings.translation.timeout_seconds != clamped {
                this.settings.translation.timeout_seconds = clamped;
                this.persist(cx);
            }
        }));

        // The key goes to the credential vault, not the config file, so it is
        // written on blur rather than per keystroke. Same trigger as the C#
        // `ApiKeyBox_LostFocus`.
        let key_focus = api_key.read(cx).focus_handle(cx);
        let this = cx.weak_entity();
        subscriptions.push(window.on_focus_out(&key_focus, cx, move |_, _, cx| {
            if let Some(view) = this.upgrade() {
                view.update(cx, |view, cx| view.commit_api_key(cx));
            }
        }));

        Self {
            page: Page::Screenshot,
            settings,
            engine: if crate::ocr::models_available() {
                "PP-OCRv6 Small".into()
            } else {
                "模型缺失，识别不可用".into()
            },
            status: SharedString::default(),
            status_error: false,
            language_open: false,
            prompt_open: false,
            recording: None,
            autostart: system::autostart_enabled(),
            saved_api_key,
            base_url,
            api_key,
            model,
            timeout,
            prompt,
            _subscriptions: subscriptions,
        }
    }

    fn ok(&mut self, text: impl Into<SharedString>) {
        self.status = text.into();
        self.status_error = false;
    }

    fn fail(&mut self, text: impl Into<SharedString>) {
        self.status = text.into();
        self.status_error = true;
    }

    fn persist(&mut self, cx: &mut Context<Self>) {
        match crate::config::save(&self.settings) {
            Ok(()) => {
                self.status = SharedString::default();
                self.status_error = false;
            }
            Err(e) => self.fail(format!("设置保存失败：{e}")),
        }
        cx.notify();
    }

    fn commit_api_key(&mut self, cx: &mut Context<Self>) {
        let key = self.api_key.read(cx).text().to_string();
        if key == self.saved_api_key {
            return;
        }
        match crate::translate::set_api_key(&key) {
            Ok(()) => {
                self.saved_api_key = key;
                self.ok("API Key 已保存");
            }
            Err(e) => self.fail(format!("API Key 保存失败：{e}")),
        }
        cx.notify();
    }

    fn toggle_toolbar_item(&mut self, id: &str, cx: &mut Context<Self>) {
        let hidden = &mut self.settings.screenshot.hidden_toolbar_items;
        match hidden.iter().position(|h| h == id) {
            Some(i) => {
                hidden.remove(i);
            }
            None => hidden.push(id.to_string()),
        }
        self.persist(cx);
    }

    fn language_label(&self) -> SharedString {
        let code = self.settings.translation.target_language.trim();
        match LANGUAGES.iter().find(|(c, _)| *c == code) {
            Some((c, name)) => format!("{name} ({c})").into(),
            // A code typed into settings.json by hand still shows as itself.
            None if code.is_empty() => "未设置".into(),
            None => code.to_string().into(),
        }
    }

    fn set_language(&mut self, code: &str, cx: &mut Context<Self>) {
        self.language_open = false;
        if self.settings.translation.target_language != code {
            self.settings.translation.target_language = code.to_string();
            self.persist(cx);
        } else {
            cx.notify();
        }
    }

    fn hotkey_text(&self, action: HotkeyAction) -> SharedString {
        if self.recording == Some(action) {
            return "按下组合键…".into();
        }
        let stored = match action {
            HotkeyAction::Region => &self.settings.hotkeys.region,
            HotkeyAction::CopyOnly => &self.settings.hotkeys.copy_only,
            HotkeyAction::Fullscreen => &self.settings.hotkeys.fullscreen,
        };
        match hotkey::parse(stored) {
            Some(h) => hotkey::format(h).into(),
            None => "未设置".into(),
        }
    }

    fn start_recording(&mut self, action: HotkeyAction, cx: &mut Context<Self>) {
        // Clicking the field that is already recording cancels it.
        if self.recording == Some(action) {
            self.cancel_recording(cx);
            return;
        }
        self.recording = Some(action);
        self.ok("按下新的组合键");
        crate::platform::start_recording();

        // Bounded no matter what happens to this window: the recorder swallows
        // bindable keys while it runs, so it must never outlive a stuck UI.
        cx.spawn(async move |this, cx| {
            Timer::after(RECORDING_TIMEOUT).await;
            crate::platform::stop_recording();
            let _ = this.update(cx, |view, cx| {
                if view.recording == Some(action) {
                    view.recording = None;
                    view.fail("录制超时，快捷键未改动");
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    fn cancel_recording(&mut self, cx: &mut Context<Self>) {
        crate::platform::stop_recording();
        self.recording = None;
        self.status = SharedString::default();
        self.status_error = false;
        cx.notify();
    }

    fn hotkey_recorded(&mut self, pressed: Hotkey, cx: &mut Context<Self>) {
        let Some(action) = self.recording.take() else {
            crate::platform::stop_recording();
            return;
        };
        crate::platform::stop_recording();

        let text = hotkey::format(pressed);
        // Two actions on one combo means one of them silently never fires.
        let clash = [
            (HotkeyAction::Region, &self.settings.hotkeys.region),
            (HotkeyAction::CopyOnly, &self.settings.hotkeys.copy_only),
            (HotkeyAction::Fullscreen, &self.settings.hotkeys.fullscreen),
        ]
        .into_iter()
        .find(|(other, stored)| *other != action && hotkey::parse(stored) == Some(pressed))
        .map(|(other, _)| other);

        if let Some(other) = clash {
            let name = HOTKEY_ROWS
                .iter()
                .find(|(a, _, _, _, _)| *a == other)
                .map(|(_, _, _, title, _)| *title)
                .unwrap_or("另一项");
            self.fail(format!("{text} 已被「{name}」占用"));
            cx.notify();
            return;
        }

        match action {
            HotkeyAction::Region => self.settings.hotkeys.region = text.clone(),
            HotkeyAction::CopyOnly => self.settings.hotkeys.copy_only = text.clone(),
            HotkeyAction::Fullscreen => self.settings.hotkeys.fullscreen = text.clone(),
        }
        self.persist(cx);
        // Takes effect now; both the LL hook and the RegisterHotKey fallback
        // hold their own copy of the bindings.
        crate::platform::apply_bindings(HotkeyBindings::from_settings(&self.settings.hotkeys));
        if self.status.is_empty() {
            self.ok(format!("已设为 {text}"));
        }
        cx.notify();
    }

    fn set_autostart(&mut self, cx: &mut Context<Self>) {
        let want = !self.autostart;
        match system::set_autostart(want) {
            Ok(()) => {
                self.status = SharedString::default();
                self.status_error = false;
            }
            Err(e) => self.fail(format!("开机启动设置失败：{e}")),
        }
        // Re-read rather than assume: the registry is the truth.
        self.autostart = system::autostart_enabled();
        cx.notify();
    }

    fn open_folder(&mut self, dir: crate::error::Result<std::path::PathBuf>, cx: &mut Context<Self>) {
        let result = dir.and_then(|path| system::open_folder(&path));
        if let Err(e) = result {
            self.fail(format!("{e}"));
        }
        cx.notify();
    }

    fn clear_logs(&mut self, cx: &mut Context<Self>) {
        match system::clear_logs() {
            Ok(0) => self.ok("没有可清理的日志"),
            Ok(n) => self.ok(format!("已清理 {n} 个日志文件")),
            Err(e) => self.fail(format!("清理失败：{e}")),
        }
        cx.notify();
    }

    fn nav(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex_none()
            .w(px(168.))
            .h_full()
            .flex()
            .flex_col()
            .bg(widgets::nav_bg())
            .child(
                div().h(px(48.)).pl(px(20.)).flex().items_center().child(
                    div()
                        .text_size(px(15.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme::primary_fg())
                        .child("Zshot"),
                ),
            )
            .child(div().h(px(8.)))
            .children(Page::ALL.map(|(page, id, icon, title)| {
                widgets::nav_item(id, icon, title, self.page == page).on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, _, cx| {
                        this.page = page;
                        cx.notify();
                    }),
                )
            }))
    }

    fn page_screenshot(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let hidden = self.settings.screenshot.hidden_toolbar_items.clone();
        let items: Vec<&'static str> = toolbar::PHASE1_ITEMS
            .iter()
            .copied()
            .filter(|id| toolbar::CUSTOMIZABLE.contains(id))
            .collect();
        // Fixed rows of four, as the C# `VariableSizedWrapGrid` lays them out.
        // Wrapping on available width reflows with the window and drifts to five.
        // Built with loops, not nested `map`s: `cx` is a `&mut` borrow and two
        // lazy closures cannot both hold it.
        let mut rows = Vec::new();
        for chunk in items.chunks(TOOLBAR_COLUMNS) {
            let mut cells = Vec::new();
            for id in chunk.iter().copied() {
                let checked = !hidden.iter().any(|h| h == id);
                cells.push(
                    widgets::checkbox(id, toolbar::label(id), checked).on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            this.toggle_toolbar_item(id, cx);
                        }),
                    ),
                );
            }
            rows.push(div().flex().flex_row().gap(px(8.)).children(cells));
        }

        div()
            .flex()
            .flex_col()
            .child(
                widgets::vcard(
                    "工具栏按钮",
                    Some("取消勾选的按钮不会出现在截图工具栏上。完成和取消始终显示。".into()),
                )
                .child(div().flex().flex_col().children(rows)),
            )
            .child(widgets::hint("工具栏改动在下一次截图时生效。"))
    }

    fn page_ocr(&self, cx: &mut Context<Self>) -> impl IntoElement {
        // No group header above this card: the page title already says it.
        let engine = widgets::card("识别引擎", Some("本地识别，截图不会离开这台机器。".into()))
            .child(
                div()
                    .flex_none()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        div()
                            .w(px(7.))
                            .h(px(7.))
                            .rounded(px(4.))
                            .bg(if crate::ocr::models_available() {
                                theme::success()
                            } else {
                                theme::danger()
                            }),
                    )
                    .child(
                        div()
                            .text_size(px(13.))
                            .text_color(theme::secondary_fg())
                            .child(self.engine.clone()),
                    ),
            );

        // Short value: it sits beside its label, with the list dropping under it.
        let current = self.settings.translation.target_language.clone();
        let language = div()
            .flex()
            .flex_col()
            .child(
                widgets::card("目标语言", None).child(
                    widgets::select(
                        "lang-select",
                        self.language_label(),
                        self.language_open,
                        Some(INLINE_FIELD_W),
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| {
                            this.language_open = !this.language_open;
                            cx.notify();
                        }),
                    ),
                ),
            )
            .when(self.language_open, |el| {
                el.child(
                    div().flex().flex_row().justify_end().child(
                        div()
                            .w(px(INLINE_FIELD_W))
                            .child(widgets::select_list().children(LANGUAGES.map(
                                |(code, name)| {
                                    widgets::select_option(
                                        code,
                                        format!("{name} ({code})"),
                                        current == code,
                                    )
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(move |this, _, _, cx| {
                                            this.set_language(code, cx)
                                        }),
                                    )
                                },
                            ))),
                    ),
                )
            });

        let prompt_summary = if self.settings.translation.prompt.trim().is_empty() {
            "使用内置"
        } else {
            "自定义"
        };
        // Collapsed by default, as the C# `SettingsExpander` has it: a prompt is
        // set once and then never looked at.
        let prompt = div()
            .flex()
            .flex_col()
            .gap(px(4.))
            .child(
                widgets::card("Prompt", Some("留空则使用内置的翻译提示词。".into()))
                    .child(widgets::expander_summary(prompt_summary, self.prompt_open))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| {
                            this.prompt_open = !this.prompt_open;
                            cx.notify();
                        }),
                    ),
            )
            .when(self.prompt_open, |el| {
                el.child(widgets::vcard("提示词", None).child(self.prompt.clone()))
            });

        div()
            .flex()
            .flex_col()
            .child(engine)
            .child(widgets::group_header("翻译"))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .child(
                        widgets::vcard(
                            "Base URL",
                            Some("OpenAI 兼容接口。留空则翻译按钮不可用。".into()),
                        )
                        .child(self.base_url.clone()),
                    )
                    .child(
                        widgets::vcard(
                            "API Key",
                            Some("保存在 Windows 凭据管理器，不写入配置文件。".into()),
                        )
                        .child(self.api_key.clone()),
                    )
                    .child(widgets::vcard("模型", None).child(self.model.clone()))
                    .child(language)
                    .child(
                        widgets::card("超时", Some("秒，5 到 300。".into())).child(
                            div()
                                .flex_none()
                                .w(px(INLINE_FIELD_W))
                                .child(self.timeout.clone()),
                        ),
                    )
                    .child(prompt),
            )
    }

    fn page_hotkey(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .children(HOTKEY_ROWS.map(|(action, id, icon, title, desc)| {
                        let recording = self.recording == Some(action);
                        widgets::icon_card(icon, title, Some(desc.into())).child(
                            div()
                                .flex_none()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(px(8.))
                                .child(
                                    widgets::hotkey_field(
                                        id,
                                        self.hotkey_text(action),
                                        recording,
                                    )
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(move |this, _, _, cx| {
                                            this.start_recording(action, cx);
                                        }),
                                    ),
                                )
                                .when(recording, |el| {
                                    el.child(
                                        widgets::cancel_button("hk-cancel").on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(|this, _, _, cx| {
                                                this.cancel_recording(cx);
                                            }),
                                        ),
                                    )
                                }),
                        )
                    })),
            )
            .child(widgets::hint(
                "点击右侧方框后按下组合键。录制期间截图不会触发；再点一次或等待 10 秒即取消。",
            ))
    }

    fn page_general(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let path_of = |p: crate::error::Result<std::path::PathBuf>| -> SharedString {
            match p {
                Ok(p) => p.display().to_string().into(),
                Err(_) => "LOCALAPPDATA 未设置".into(),
            }
        };
        div()
            .flex()
            .flex_col()
            .child(widgets::group_header("启动").mt(px(0.)))
            .child(
                widgets::card(
                    "开机时启动",
                    Some("写入当前用户的 Run 注册表项，不需要管理员权限。".into()),
                )
                .child(
                    widgets::toggle("autostart", self.autostart).on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| this.set_autostart(cx)),
                    ),
                ),
            )
            .child(widgets::group_header("维护"))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .child(
                        widgets::card("数据目录", None)
                            .child(
                                div()
                                    .flex_none()
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .gap(px(12.))
                                    .child(widgets::path_line(path_of(system::data_dir())))
                                    .child(
                                        widgets::icon_button("open-data", "settings/folder.svg")
                                            .on_mouse_down(
                                                MouseButton::Left,
                                                cx.listener(|this, _, _, cx| {
                                                    this.open_folder(system::data_dir(), cx);
                                                }),
                                            ),
                                    ),
                            ),
                    )
                    .child(
                        widgets::card("日志目录", None).child(
                            div()
                                .flex_none()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(px(12.))
                                .child(widgets::path_line(path_of(crate::perf::log_dir())))
                                .child(
                                    widgets::icon_button("open-logs", "settings/folder.svg")
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(|this, _, _, cx| {
                                                this.open_folder(crate::perf::log_dir(), cx);
                                            }),
                                        ),
                                ),
                        ),
                    )
                    .child(
                        widgets::card(
                            "清理日志",
                            Some("删除 perf.log 等运行日志，不影响设置和截图。".into()),
                        )
                        .child(
                            widgets::button("clear-logs", "清理").on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _, _, cx| this.clear_logs(cx)),
                            ),
                        ),
                    ),
            )
    }
}

impl Render for SettingsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let page = match self.page {
            Page::Screenshot => self.page_screenshot(cx).into_any_element(),
            Page::Ocr => self.page_ocr(cx).into_any_element(),
            Page::Hotkey => self.page_hotkey(cx).into_any_element(),
            Page::General => self.page_general(cx).into_any_element(),
        };
        let header = widgets::page_header(self.page.title()).when(!self.status.is_empty(), |el| {
            el.child(widgets::status_pill(self.status.clone(), self.status_error))
        });

        let body = div()
            .id("settings-page")
            .flex_1()
            .h_full()
            .overflow_y_scroll()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .pb(px(24.))
                    .px(px(24.))
                    .child(header)
                    .child(
                        div()
                            .w_full()
                            .max_w(px(CONTENT_MAX_W))
                            .flex()
                            .flex_col()
                            .child(page),
                    ),
            );

        div()
            .flex()
            .flex_row()
            .size_full()
            .bg(widgets::page_bg())
            .text_color(theme::primary_fg())
            .child(self.nav(cx))
            .child(body)
    }
}
