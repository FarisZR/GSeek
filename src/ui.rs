use crate::{
    APP_ID,
    config::{Backend, External, Paths, Settings},
    integration, service,
};
use adw::prelude::*;
use anyhow::Result;
use glib::variant::ToVariant;
use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    rc::Rc,
    time::Duration,
};

struct Window {
    window: adw::ApplicationWindow,
    main_stack: gtk::Stack,
    search_entry: gtk::SearchEntry,
    search_results: gio::ListStore,
    search_content: gtk::Stack,
    search_placeholder: adw::StatusPage,
    search_status: gtk::Label,
    search_generation: Cell<u64>,
    search_refresh_pending: Cell<bool>,
    banner: adw::Banner,
    status_title: gtk::Label,
    status_detail: gtk::Label,
    counts: gtk::Label,
    progress: gtk::ProgressBar,
    current: gtk::Label,
    backend: gtk::Label,
    backend_views: RefCell<Vec<BackendView>>,
    last_status: RefCell<service::Status>,
    settings_queue: RefCell<VecDeque<SettingsRequest>>,
    settings_busy: Cell<bool>,
    settings_hold: RefCell<Option<gio::ApplicationHoldGuard>>,
    logs: gtk::ListBox,
    pause: gtk::Button,
    refreshing: Cell<bool>,
    paths: Paths,
}

struct SettingsWrite {
    method: &'static str,
    value: serde_json::Value,
    complete: Option<Box<dyn FnOnce(bool)>>,
}

type SettingsLoaded = Box<dyn FnOnce(&Rc<Window>, Settings)>;
enum SettingsRequest {
    Write(SettingsWrite),
    Read(SettingsLoaded),
}

struct BackendView {
    active: glib::WeakRef<adw::ActionRow>,
    detail: glib::WeakRef<adw::ActionRow>,
    retry: glib::WeakRef<gtk::Button>,
}
impl BackendView {
    fn refresh(&self, status: &service::Status) -> bool {
        let (Some(active), Some(detail), Some(retry)) = (
            self.active.upgrade(),
            self.detail.upgrade(),
            self.retry.upgrade(),
        ) else {
            return false;
        };
        active.set_subtitle(&status.backend_description());
        detail.set_subtitle(&status.detail);
        detail.set_visible(!status.ready);
        retry.set_visible(status.state == "error");
        true
    }
}

pub fn run(paths: Paths, query: Option<String>) -> Result<()> {
    let app = adw::Application::builder()
        .application_id(APP_ID)
        .flags(gio::ApplicationFlags::HANDLES_COMMAND_LINE)
        .build();
    app.add_main_option(
        "query",
        0u8.into(),
        glib::OptionFlags::NONE,
        glib::OptionArg::String,
        "Show matching files",
        Some("TEXT"),
    );
    let current: Rc<RefCell<Option<Rc<Window>>>> = Rc::new(RefCell::new(None));
    let state = current.clone();
    let app_paths = paths.clone();
    app.connect_activate(move |app| {
        if let Some(ui) = state.borrow().as_ref() {
            ui.window.present();
            return;
        }
        let ui = Window::build(app, app_paths.clone());
        ui.window.present();
        ui.refresh();
        let weak = Rc::downgrade(&ui);
        glib::timeout_add_local(Duration::from_secs(1), move || {
            if let Some(ui) = weak.upgrade() {
                if ui.window.is_visible() {
                    ui.refresh();
                }
                glib::ControlFlow::Continue
            } else {
                glib::ControlFlow::Break
            }
        });
        *state.borrow_mut() = Some(ui);
    });
    let state = current.clone();
    let search = gio::SimpleAction::new_stateful(
        "search",
        Some(&String::static_variant_type()),
        &"".to_variant(),
    );
    let weak_app = app.downgrade();
    search.connect_activate(move |action, value| {
        if let Some(query) = value.and_then(|v| v.str())
            && let Some(app) = weak_app.upgrade()
        {
            app.activate();
            action.set_state(&query.to_variant());
            if let Some(ui) = state.borrow().as_ref() {
                ui.show_results(query);
            }
        }
    });
    app.add_action(&search);
    let quit = gio::SimpleAction::new("quit", None);
    let weak_app = app.downgrade();
    let state = current.clone();
    quit.connect_activate(move |_, _| {
        if let Some(ui) = state.borrow().as_ref() {
            ui.window.close();
        } else if let Some(app) = weak_app.upgrade() {
            app.quit();
        }
    });
    app.add_action(&quit);
    app.set_accels_for_action("app.quit", &["<Primary>q", "<Primary>w"]);
    app.connect_command_line(|app, command| {
        if let Ok(Some(query)) = command.options_dict().lookup::<String>("query") {
            app.activate_action("search", Some(&query.to_variant()));
        } else {
            app.activate();
        }
        glib::ExitCode::SUCCESS
    });
    let arguments = std::iter::once("gseek".to_owned())
        .chain(query.map(|query| format!("--query={query}")))
        .collect::<Vec<_>>();
    let exit = app.run_with_args(&arguments);
    if exit != glib::ExitCode::SUCCESS {
        anyhow::bail!("Application exited with {exit:?}");
    }
    Ok(())
}

fn vertical(spacing: i32) -> gtk::Box {
    gtk::Box::new(gtk::Orientation::Vertical, spacing)
}
fn label(text: &str, class: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.add_css_class(class);
    label.set_wrap(true);
    label
}
fn margins(widget: &impl IsA<gtk::Widget>, value: i32) {
    widget.set_margin_top(value);
    widget.set_margin_bottom(value);
    widget.set_margin_start(value);
    widget.set_margin_end(value);
}

impl Window {
    fn build(app: &adw::Application, paths: Paths) -> Rc<Self> {
        static RESOURCES: std::sync::Once = std::sync::Once::new();
        RESOURCES.call_once(|| {
            gio::resources_register_include!("gseek.gresource")
                .expect("Valid embedded GTK resources");
        });
        let display = gtk::gdk::Display::default().expect("GTK has a display");
        gtk::IconTheme::for_display(&display).add_resource_path("/io/github/FarisZR/GSeek/icons");
        gtk::Window::set_default_icon_name(APP_ID);
        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title("GSeek")
            .default_width(720)
            .default_height(700)
            .build();
        let root = adw::ToolbarView::new();
        let header = adw::HeaderBar::new();
        let menu = gio::Menu::new();
        menu.append(Some("Preferences"), Some("app.preferences"));
        menu.append(Some("Search Files"), Some("app.results"));
        menu.append(Some("Indexing & Activity"), Some("app.activity"));
        menu.append(Some("Restart GSeek"), Some("app.restart"));
        menu.append(Some("About GSeek"), Some("app.about"));
        let menu_button = gtk::MenuButton::builder()
            .icon_name("open-menu-symbolic")
            .menu_model(&menu)
            .tooltip_text("Main Menu")
            .build();
        header.pack_end(&menu_button);
        let back = gtk::Button::builder()
            .icon_name("go-previous-symbolic")
            .tooltip_text("Back")
            .visible(false)
            .build();
        header.pack_start(&back);
        root.add_top_bar(&header);
        let main_stack = gtk::Stack::new();
        root.set_content(Some(&main_stack));
        window.set_content(Some(&root));

        let welcome = adw::StatusPage::builder()
            .paintable(&gtk::gdk::Texture::from_resource(
                "/io/github/FarisZR/GSeek/icons/512x512/apps/io.github.FarisZR.GSeek.png",
            ))
            .title("Set Up GSeek")
            .description("Search documents, photos, and recordings from the Activities overview")
            .build();
        let welcome_box = vertical(24);
        let privacy = label("Files stay on this computer", "dim-label");
        privacy.set_justify(gtk::Justification::Center);
        privacy.set_xalign(0.5);
        welcome_box.append(&privacy);
        let checklist = label(
            "✓ GSeek installed\nDownload EmbeddingGemma 2 (865 MB)\nDownload the inference runtime\nSelect a backend automatically",
            "dim-label",
        );
        checklist.set_justify(gtk::Justification::Center);
        checklist.set_xalign(0.5);
        welcome_box.append(&checklist);
        let start = gtk::Button::with_label("Choose Folders");
        start.add_css_class("suggested-action");
        start.add_css_class("pill");
        start.set_halign(gtk::Align::Center);
        welcome_box.append(&start);
        welcome.set_child(Some(&welcome_box));
        main_stack.add_named(&welcome, Some("welcome"));

        let body = vertical(0);
        let banner = adw::Banner::builder()
            .use_markup(false)
            .title("Setting up semantic search")
            .revealed(false)
            .build();
        body.append(&banner);
        main_stack.add_named(&body, Some("library"));

        let activity_box = vertical(20);
        margins(&activity_box, 24);
        let status_title = label("Preparing search", "title-1");
        status_title.set_halign(gtk::Align::Start);
        activity_box.append(&status_title);
        let status_detail = label("", "dim-label");
        status_detail.set_xalign(0.0);
        status_detail.set_selectable(true);
        activity_box.append(&status_detail);
        let backend_status = label("", "dim-label");
        backend_status.set_xalign(0.0);
        activity_box.append(&backend_status);
        let hint = label("Press Super and describe what you remember.", "dim-label");
        hint.set_xalign(0.0);
        activity_box.append(&hint);
        let progress = gtk::ProgressBar::new();
        activity_box.append(&progress);
        let counts = label("", "heading");
        counts.set_xalign(0.0);
        activity_box.append(&counts);
        let current_file = label("", "monospace");
        current_file.set_xalign(0.0);
        current_file.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        current_file.set_wrap(false);
        activity_box.append(&current_file);
        let buttons = gtk::FlowBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .min_children_per_line(1)
            .max_children_per_line(3)
            .column_spacing(8)
            .row_spacing(8)
            .halign(gtk::Align::Start)
            .build();
        let pause = gtk::Button::with_label("Pause Indexing");
        buttons.insert(&pause, -1);
        let scan = gtk::Button::with_label("Index Now");
        buttons.insert(&scan, -1);
        let retry = gtk::Button::with_label("Retry Failed");
        buttons.insert(&retry, -1);
        activity_box.append(&buttons);
        let logs = gtk::ListBox::new();
        logs.add_css_class("boxed-list");
        logs.set_selection_mode(gtk::SelectionMode::None);
        let log_label = label("Recent Activity", "heading");
        log_label.set_xalign(0.0);
        activity_box.append(&log_label);
        let log_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&logs)
            .build();
        activity_box.append(&log_scroll);
        let activity_clamp = adw::Clamp::builder()
            .maximum_size(800)
            .child(&activity_box)
            .build();
        body.append(&activity_clamp);
        let back_button = back.clone();
        main_stack.connect_visible_child_name_notify(move |stack| {
            back.set_visible(matches!(
                stack.visible_child_name().as_deref(),
                Some("setup" | "results")
            ));
        });
        let search_box = vertical(16);
        margins(&search_box, 24);
        let search_entry = gtk::SearchEntry::builder()
            .placeholder_text("Search Files")
            .hexpand(true)
            .build();
        search_box.append(&search_entry);
        let search_status = label("Enter a filename or describe its contents", "dim-label");
        search_status.set_xalign(0.0);
        search_box.append(&search_status);
        let search_results = gio::ListStore::new::<glib::BoxedAnyObject>();
        let factory = gtk::SignalListItemFactory::new();
        factory.connect_bind(|_, item| {
            let item = item.downcast_ref::<gtk::ListItem>().unwrap();
            let object = item
                .item()
                .unwrap()
                .downcast::<glib::BoxedAnyObject>()
                .unwrap();
            let hit = object.borrow::<crate::store::SearchHit>();
            let subtitle = if hit.snippet.is_empty() {
                hit.path
                    .parent()
                    .unwrap_or(&hit.path)
                    .to_string_lossy()
                    .into_owned()
            } else {
                hit.snippet.clone()
            };
            let row = adw::ActionRow::builder()
                .title(glib::markup_escape_text(&hit.name))
                .subtitle(glib::markup_escape_text(&subtitle))
                .build();
            row.set_title_lines(1);
            row.set_subtitle_lines(2);
            let image = gtk::Image::from_gicon(&integration::icon_for(&hit.path, &hit.mime));
            image.set_pixel_size(32);
            row.add_prefix(&image);
            item.set_child(Some(&row));
        });
        let selection = gtk::NoSelection::new(Some(search_results.clone()));
        let result_list = gtk::ListView::new(Some(selection), Some(factory));
        result_list.add_css_class("boxed-list");
        result_list.set_single_click_activate(true);
        let scroll = gtk::ScrolledWindow::builder()
            .vexpand(true)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&result_list)
            .build();
        let search_content = gtk::Stack::new();
        let search_placeholder = adw::StatusPage::builder()
            .icon_name("system-search-symbolic")
            .title("Search Files")
            .description("Enter a filename or describe its contents")
            .vexpand(true)
            .build();
        search_content.add_named(&search_placeholder, Some("empty"));
        search_content.add_named(&scroll, Some("results"));
        search_box.append(&search_content);
        let search_clamp = adw::Clamp::builder()
            .maximum_size(800)
            .child(&search_box)
            .build();
        main_stack.add_named(&search_clamp, Some("results"));
        let ui = Rc::new(Self {
            window,
            main_stack,
            search_entry,
            search_results,
            search_content,
            search_placeholder,
            search_status,
            search_generation: Cell::new(0),
            search_refresh_pending: Cell::new(false),
            banner,
            status_title,
            status_detail,
            counts,
            progress,
            current: current_file,
            backend: backend_status,
            backend_views: RefCell::new(Vec::new()),
            last_status: RefCell::new(service::Status::default()),
            settings_queue: RefCell::new(VecDeque::new()),
            settings_busy: Cell::new(false),
            settings_hold: RefCell::new(None),
            logs,
            pause,
            refreshing: Cell::new(false),
            paths,
        });
        let weak = Rc::downgrade(&ui);
        result_list.connect_activate(move |_, position| {
            if let Some(ui) = weak.upgrade()
                && let Some(item) = ui.search_results.item(position)
            {
                let object = item.downcast::<glib::BoxedAnyObject>().unwrap();
                let uri = object.borrow::<crate::store::SearchHit>().uri.clone();
                let paths = ui.paths.clone();
                let weak = Rc::downgrade(&ui);
                spawn(
                    move || integration::open_result(&paths, &uri),
                    move |result| {
                        if let (Err(e), Some(ui)) = (result, weak.upgrade()) {
                            ui.error(&format!("{e:#}"));
                        }
                    },
                );
            }
        });
        let weak = Rc::downgrade(&ui);
        ui.search_entry.connect_search_changed(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.search_files();
            }
        });
        let weak = Rc::downgrade(&ui);
        ui.search_entry.connect_activate(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.search_files();
            }
        });
        let weak = Rc::downgrade(&ui);
        let results = gio::SimpleAction::new("results", None);
        results.connect_activate(move |_, _| {
            if let Some(ui) = weak.upgrade() {
                ui.show_results(&ui.search_entry.text());
            }
        });
        app.add_action(&results);
        app.set_accels_for_action("app.results", &["<Primary>f"]);
        let weak = Rc::downgrade(&ui);
        let activity = gio::SimpleAction::new("activity", None);
        activity.connect_activate(move |_, _| {
            if let Some(ui) = weak.upgrade() {
                ui.main_stack.set_visible_child_name("library");
                ui.refresh();
            }
        });
        app.add_action(&activity);

        let weak = Rc::downgrade(&ui);
        back_button.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.with_settings(|ui, settings| {
                    ui.main_stack
                        .set_visible_child_name(if settings.configured {
                            "library"
                        } else {
                            "welcome"
                        });
                });
            }
        });
        let weak = Rc::downgrade(&ui);
        start.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.setup();
            }
        });
        let weak = Rc::downgrade(&ui);
        ui.pause.connect_clicked(move |button| {
            let command = if button.label().as_deref() == Some("Resume Indexing") {
                "resume"
            } else {
                "pause"
            };
            if let Some(ui) = weak.upgrade() {
                ui.action(command, "");
            }
        });
        for (button, command) in [(scan, "scan"), (retry, "retry")] {
            let weak = Rc::downgrade(&ui);
            button.connect_clicked(move |_| {
                if let Some(ui) = weak.upgrade() {
                    ui.action(command, "");
                }
            });
        }
        let preferences = gio::SimpleAction::new("preferences", None);
        let weak = Rc::downgrade(&ui);
        preferences.connect_activate(move |_, _| {
            if let Some(ui) = weak.upgrade() {
                ui.preferences(None);
            }
        });
        app.add_action(&preferences);
        let restart = gio::SimpleAction::new("restart", None);
        let weak = Rc::downgrade(&ui);
        restart.connect_activate(move |action, _| {
            action.set_enabled(false);
            let weak = weak.clone();
            let action = action.clone();
            // The helper waits for this application to release its bus name before
            // launching the stable installed path. Keep GTK free to save and quit.
            spawn(
                || {
                    let output = std::process::Command::new(service::launcher()?)
                        .arg("restart")
                        .output()?;
                    anyhow::ensure!(
                        output.status.success(),
                        "Could not restart GSeek: {}",
                        String::from_utf8_lossy(&output.stderr).trim()
                    );
                    Ok(())
                },
                move |result| {
                    action.set_enabled(true);
                    if let Err(error) = result
                        && let Some(ui) = weak.upgrade()
                    {
                        ui.error(&format!("{error:#}"));
                    }
                },
            );
        });
        app.add_action(&restart);
        let about = gio::SimpleAction::new("about", None);
        let weak = Rc::downgrade(&ui);
        about.connect_activate(move |_,_|{if let Some(ui)=weak.upgrade(){
            let dialog=adw::AboutDialog::builder().application_name("GSeek").application_icon(APP_ID).version(env!("CARGO_PKG_VERSION"))
                .developer_name("FarisZR").website("https://github.com/FarisZR/GSeek").issue_url("https://github.com/FarisZR/GSeek/issues")
                .license_type(gtk::License::Gpl30).comments("Private, multilingual file search for GNOME. Powered by EmbeddingGemma 2 and llama.cpp.").build();dialog.present(Some(&ui.window));
        }});
        app.add_action(&about);
        app.set_accels_for_action("app.preferences", &["<Primary>comma"]);
        ui
    }

    fn show_results(self: &Rc<Self>, query: &str) {
        self.main_stack.set_visible_child_name("results");
        if self.search_entry.text() == query {
            self.search_files();
        } else {
            self.search_entry.set_text(query);
        }
        self.search_entry.grab_focus();
        self.window.present();
    }

    fn search_files(self: &Rc<Self>) {
        let query = self.search_entry.text().trim().to_owned();
        let generation = self.search_generation.get().wrapping_add(1);
        self.search_generation.set(generation);
        self.search_results.remove_all();
        self.search_content.set_visible_child_name("empty");
        self.search_status
            .set_text("Includes content matches and standard GNOME filename results");
        if query.is_empty() {
            self.search_placeholder.set_title("Search Files");
            self.search_placeholder
                .set_description(Some("Enter a filename or describe its contents"));
            return;
        }
        self.search_placeholder.set_title("Searching…");
        self.search_placeholder.set_description(None);
        let weak = Rc::downgrade(self);
        let requested = query.clone();
        spawn(
            move || -> Result<_> {
                let hits = service::search(&requested, crate::store::MAX_RESULTS as u32)?;
                let warning = service::read_status()
                    .map(|s| s.filename_error)
                    .unwrap_or_default();
                Ok((hits, warning))
            },
            move |result| {
                let Some(ui) = weak.upgrade() else {
                    return;
                };
                if ui.search_generation.get() != generation
                    || ui.search_entry.text().trim() != query
                {
                    return;
                }
                match result {
                    Ok((hits, warning)) if hits.is_empty() => {
                        ui.search_placeholder.set_title("No Results");
                        ui.search_placeholder
                            .set_description(Some(if warning.is_empty() {
                                "Try another filename or description"
                            } else {
                                &warning
                            }));
                    }
                    Ok((hits, warning)) => {
                        ui.search_status.set_text(&if hits.len() == crate::store::MAX_RESULTS {
                        "Showing the first 5000 matches · Refine the search for more specific results".into()
                    } else { format!("{} results · Includes standard GNOME filename matches", hits.len()) });
                        if !warning.is_empty() {
                            ui.search_status
                                .set_text(&format!("{} results · {warning}", hits.len()));
                        }
                        for hit in hits {
                            ui.search_results.append(&glib::BoxedAnyObject::new(hit));
                        }
                        ui.search_content.set_visible_child_name("results");
                    }
                    Err(e) => {
                        ui.search_placeholder.set_title("Search Unavailable");
                        ui.search_placeholder
                            .set_description(Some(&format!("{e:#}")));
                    }
                }
            },
        );
    }

    fn error(&self, message: &str) {
        let dialog = adw::AlertDialog::builder()
            .heading("GSeek needs your attention")
            .body(message)
            .build();
        dialog.add_response("close", "Close");
        dialog.present(Some(&self.window));
    }

    fn action(self: &Rc<Self>, command: &str, argument: &str) {
        let command = command.to_owned();
        let argument = argument.to_owned();
        let weak = Rc::downgrade(self);
        spawn(
            move || service::command(&command, &argument),
            move |result| {
                if let Some(ui) = weak.upgrade() {
                    if let Err(e) = result {
                        ui.error(&format!("{e:#}"));
                    }
                    ui.refresh();
                }
            },
        );
    }

    fn refresh(self: &Rc<Self>) {
        if self.refreshing.replace(true) {
            return;
        }
        let weak = Rc::downgrade(self);
        spawn(
            || {
                Ok((
                    service::read_status()?,
                    service::read_settings()?.configured,
                ))
            },
            move |result| {
                if let Some(ui) = weak.upgrade() {
                    ui.refreshing.set(false);
                    match result {
                        Ok((s, configured)) => {
                            if !matches!(
                                ui.main_stack.visible_child_name().as_deref(),
                                Some("setup" | "results")
                            ) {
                                ui.main_stack.set_visible_child_name(
                                    if configured || s.state == "setup" || s.state == "error" {
                                        "library"
                                    } else {
                                        "welcome"
                                    },
                                );
                            }
                            ui.banner.set_revealed(
                                !s.ready || s.state == "error" || !s.discovery_error.is_empty(),
                            );
                            ui.banner.set_title(if s.discovery_error.is_empty() {
                                &s.detail
                            } else {
                                &s.discovery_error
                            });
                            let title = match s.state.as_str() {
                                "setup" => "Setting Up Search",
                                "discovering" => "Finding Your Files",
                                "indexing" => "Building Your Search Library",
                                "paused" => "Indexing Paused",
                                "ready" => "Ready to Search",
                                "error" => "Search Needs Attention",
                                _ => "Welcome to GSeek",
                            };
                            ui.status_title.set_text(title);
                            ui.status_detail.set_text(&if s.discovery_error.is_empty() {
                                s.detail.clone()
                            } else {
                                format!("{}\n{}", s.detail, s.discovery_error)
                            });
                            ui.counts.set_text(&format!(
                                "{} files · {} indexed · {} queued · {} failed",
                                s.counts.files, s.counts.indexed, s.counts.pending, s.counts.failed
                            ));
                            ui.progress.set_fraction(if s.state == "setup" {
                                s.progress.clamp(0.0, 1.0)
                            } else if s.counts.files > 0 {
                                (s.counts.indexed + s.counts.skipped) as f64 / s.counts.files as f64
                            } else {
                                0.0
                            });
                            ui.current.set_text(&s.current);
                            let backend = s.backend_description();
                            ui.backend.set_text(&format!("Backend: {backend}"));
                            ui.backend_views
                                .borrow_mut()
                                .retain(|view| view.refresh(&s));
                            *ui.last_status.borrow_mut() = s.clone();
                            ui.pause.set_label(if s.paused {
                                "Resume Indexing"
                            } else {
                                "Pause Indexing"
                            });
                            if ui.search_refresh_pending.get()
                                && ui.main_stack.visible_child_name().as_deref() == Some("results")
                            {
                                if s.ready
                                    && !ui.settings_busy.get()
                                    && ui.settings_queue.borrow().is_empty()
                                {
                                    ui.search_refresh_pending.set(false);
                                    ui.search_files();
                                } else if !s.ready {
                                    ui.search_placeholder.set_title(if s.state == "error" {
                                        "Search Unavailable"
                                    } else {
                                        "Updating Search"
                                    });
                                    ui.search_placeholder.set_description(Some(&s.detail));
                                }
                            }
                        }
                        Err(e) => {
                            ui.banner.set_revealed(true);
                            ui.banner.set_title("Cannot connect to background search. Open Preferences to repair setup.");
                            ui.status_detail.set_text(&format!("{e:#}"));
                        }
                    }
                }
            },
        );
        {
            let weak = Rc::downgrade(self);
            spawn(service::read_activity, move |result| {
                if let Some(ui) = weak.upgrade()
                    && let Ok(logs) = result
                {
                    clear_list(&ui.logs);
                    for item in logs.iter().take(100) {
                        let path = url::Url::parse(&item.path)
                            .ok()
                            .and_then(|u| u.to_file_path().ok())
                            .map(|p| p.to_string_lossy().into_owned())
                            .unwrap_or_else(|| item.path.clone());
                        let row = adw::ActionRow::builder()
                            .title(glib::markup_escape_text(&path))
                            .subtitle(glib::markup_escape_text(&item.detail))
                            .build();
                        row.set_title_lines(1);
                        row.set_subtitle_lines(2);
                        let icon = gtk::Image::from_icon_name(if item.state == "indexed" {
                            "object-select-symbolic"
                        } else {
                            "dialog-warning-symbolic"
                        });
                        row.add_prefix(&icon);
                        ui.logs.append(&row);
                    }
                }
            });
        }
    }

    fn apply_settings(self: &Rc<Self>, settings: Settings, button: &gtk::Button) {
        if settings.folders.is_empty() {
            self.error("Choose at least one search folder.");
            return;
        }
        if let Err(error) = settings.validate() {
            self.error(&format!("{error:#}"));
            return;
        }
        button.set_sensitive(false);
        let weak = Rc::downgrade(self);
        let button = button.clone();
        self.queue_settings(SettingsRequest::Write(SettingsWrite {
            method: "Configure",
            value: serde_json::to_value(settings).unwrap(),
            complete: Some(Box::new(move |saved| {
                button.set_sensitive(true);
                if saved && let Some(ui) = weak.upgrade() {
                    ui.main_stack.set_visible_child_name("library");
                    ui.refresh();
                }
            })),
        }));
    }

    fn save_preference(self: &Rc<Self>, patch: serde_json::Value) {
        self.queue_settings(SettingsRequest::Write(SettingsWrite {
            method: "UpdateSettings",
            value: patch,
            complete: None,
        }));
    }
    fn with_settings(self: &Rc<Self>, complete: impl FnOnce(&Rc<Self>, Settings) + 'static) {
        // Read through the same queue as writes: reopening a page can never
        // observe an older save or another process's XDG configuration directory.
        self.queue_settings(SettingsRequest::Read(Box::new(complete)));
    }
    fn queue_settings(self: &Rc<Self>, request: SettingsRequest) {
        if self.settings_hold.borrow().is_none() {
            *self.settings_hold.borrow_mut() = self.window.application().map(|app| app.hold());
        }
        self.settings_queue.borrow_mut().push_back(request);
        self.process_settings();
    }
    fn process_settings(self: &Rc<Self>) {
        if self.settings_busy.get() {
            return;
        }
        let Some(request) = self.settings_queue.borrow_mut().pop_front() else {
            self.settings_hold.borrow_mut().take();
            return;
        };
        self.settings_busy.set(true);
        let request = match request {
            SettingsRequest::Write(request) => request,
            SettingsRequest::Read(complete) => {
                let weak = Rc::downgrade(self);
                spawn(service::read_settings, move |result| {
                    if let Some(ui) = weak.upgrade() {
                        ui.settings_busy.set(false);
                        match result {
                            Ok(settings) => complete(&ui, settings),
                            Err(error) => {
                                ui.error(&format!("Could not load preferences: {error:#}"))
                            }
                        }
                        ui.process_settings();
                    }
                });
                return;
            }
        };
        let paths = self.paths.clone();
        let weak = Rc::downgrade(self);
        let method = request.method;
        let value = request.value;
        spawn(
            move || {
                if method == "Configure" {
                    integration::install(&paths, &service::launcher()?)?;
                }
                service::call(method, Some(&(value.to_string(),).to_variant()))?;
                Ok(())
            },
            move |result| {
                if let Some(complete) = request.complete {
                    complete(result.is_ok());
                }
                if let Some(ui) = weak.upgrade() {
                    ui.settings_busy.set(false);
                    if let Err(error) = result {
                        ui.error(&format!("Could not save preference: {error:#}"));
                    } else if ui.main_stack.visible_child_name().as_deref() == Some("results") {
                        ui.search_refresh_pending.set(true);
                        ui.search_generation
                            .set(ui.search_generation.get().wrapping_add(1));
                        ui.search_results.remove_all();
                        ui.search_content.set_visible_child_name("empty");
                        ui.search_placeholder.set_title("Updating Search");
                        ui.search_placeholder
                            .set_description(Some("Applying preferences"));
                    }
                    ui.refresh();
                    ui.process_settings();
                }
            },
        );
    }

    fn setup(self: &Rc<Self>) {
        self.with_settings(|ui, settings| ui.setup_with_settings(settings));
    }

    fn setup_with_settings(self: &Rc<Self>, settings: Settings) {
        let draft = Rc::new(RefCell::new(settings));
        if let Some(previous) = self.main_stack.child_by_name("setup") {
            self.main_stack.remove(&previous);
        }
        let toolbar = adw::ToolbarView::new();
        let content = vertical(24);
        margins(&content, 24);
        let intro = label("Set Up GSeek", "title-1");
        intro.set_xalign(0.0);
        content.append(&intro);
        let description = label(
            "Choose folders for content search. Files elsewhere in GNOME’s search locations remain searchable by name.",
            "dim-label",
        );
        description.set_xalign(0.0);
        content.append(&description);
        let locations = adw::PreferencesGroup::builder()
            .title("Choose Folders")
            .description("Only files also included in GNOME’s Search Locations can be embedded.")
            .build();
        let folders = gtk::ListBox::new();
        folders.add_css_class("boxed-list");
        folders.set_selection_mode(gtk::SelectionMode::None);
        fill_folders(&folders, &draft, false, self);
        locations.add(&folders);
        let add = gtk::Button::with_label("Add Folders…");
        add.set_valign(gtk::Align::Center);
        locations.set_header_suffix(Some(&add));
        connect_folder_picker(&add, &self.window, &folders, &draft, false, self);
        content.append(&locations);
        content.append(&default_exclusions_group());
        content.append(&reader_status_group());
        let group = adw::PreferencesGroup::new();
        let replace = adw::SwitchRow::builder()
            .title("Use GSeek in GNOME Search")
            .subtitle("Enable GSeek and disable the Files provider after the model is ready.")
            .active(true)
            .build();
        group.add(&replace);
        content.append(&group);
        let privacy = label(
            "No account. No telemetry. Files stay on this computer.",
            "dim-label",
        );
        privacy.set_xalign(0.0);
        content.append(&privacy);
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&content)
            .build();
        let clamp = adw::Clamp::builder()
            .maximum_size(600)
            .child(&scroller)
            .build();
        toolbar.set_content(Some(&clamp));
        let footer = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        margins(&footer, 16);
        let advanced = gtk::Button::with_label("Preferences");
        advanced.add_css_class("flat");
        footer.append(&advanced);
        let start = gtk::Button::with_label("Download & Start");
        start.add_css_class("suggested-action");
        start.set_hexpand(true);
        footer.append(&start);
        let footer_clamp = adw::Clamp::builder()
            .maximum_size(600)
            .child(&footer)
            .build();
        toolbar.add_bottom_bar(&footer_clamp);
        self.main_stack.add_named(&toolbar, Some("setup"));
        let weak = Rc::downgrade(self);
        let settings = draft.clone();
        start.connect_clicked(move |button| {
            if let Some(ui) = weak.upgrade() {
                let mut s = settings.borrow().clone();
                s.search_enabled = replace.is_active();
                ui.apply_settings(s, button);
            }
        });
        let weak = Rc::downgrade(self);
        advanced.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.with_settings(|ui, settings| {
                    ui.preferences(Some(settings));
                    if let Some(dialog) = ui.window.visible_dialog() {
                        let weak = Rc::downgrade(ui);
                        dialog.connect_closed(move |_| {
                            if let Some(ui) = weak.upgrade()
                                && ui.main_stack.visible_child_name().as_deref() == Some("setup")
                            {
                                ui.setup(); // Queued after the dialog's pending saves.
                            }
                        });
                    }
                });
            }
        });
        self.main_stack.set_visible_child_name("setup");
        add.grab_focus();
    }

    fn preferences(self: &Rc<Self>, initial: Option<Settings>) {
        let Some(settings) = initial else {
            self.with_settings(|ui, settings| ui.preferences(Some(settings)));
            return;
        };
        let draft = Rc::new(RefCell::new(settings));
        let dialog = adw::PreferencesDialog::builder()
            .title("Preferences")
            .content_width(680)
            .content_height(650)
            .build();
        let page = adw::PreferencesPage::builder()
            .title("Search")
            .icon_name("system-search-symbolic")
            .build();
        let locations=adw::PreferencesGroup::builder().title("Content Search Folders").description("Embed content from these folders in GNOME’s search locations. Other files remain searchable by name. Hidden files, credentials, caches, and build outputs are excluded.").build();
        let folder_list = gtk::ListBox::new();
        folder_list.add_css_class("boxed-list");
        folder_list.set_selection_mode(gtk::SelectionMode::None);
        fill_folders(&folder_list, &draft, false, self);
        locations.add(&folder_list);
        let add = gtk::Button::with_label("Add Folders…");
        add.set_valign(gtk::Align::Center);
        locations.set_header_suffix(Some(&add));
        connect_folder_picker(&add, &self.window, &folder_list, &draft, false, self);
        let scope = adw::ActionRow::builder()
            .title("GNOME Search Locations")
            .subtitle("Open Settings if a selected folder does not appear in the index")
            .activatable(true)
            .build();
        scope.add_suffix(&gtk::Image::from_icon_name("go-jump-symbolic"));
        scope.connect_activated(|_| {
            let _ = std::process::Command::new("gnome-control-center")
                .arg("search")
                .spawn();
        });

        page.add(&locations);
        let exclusions = adw::PreferencesGroup::builder()
            .title("Excluded Folders")
            .build();
        let exclude_list = gtk::ListBox::new();
        exclude_list.add_css_class("boxed-list");
        exclude_list.set_selection_mode(gtk::SelectionMode::None);
        fill_folders(&exclude_list, &draft, true, self);
        exclusions.add(&exclude_list);
        let exclude = gtk::Button::with_label("Exclude Folders…");
        exclude.set_valign(gtk::Align::Center);
        exclusions.set_header_suffix(Some(&exclude));
        connect_folder_picker(&exclude, &self.window, &exclude_list, &draft, true, self);
        page.add(&exclusions);
        page.add(&default_exclusions_group());
        let types = adw::PreferencesGroup::builder().title("Content").build();
        let images = adw::SwitchRow::builder()
            .title("Images and Screenshots")
            .subtitle("Find pictures by what they show")
            .active(draft.borrow().images)
            .build();
        types.add(&images);
        let media = adw::SwitchRow::builder()
            .title("Audio and Video")
            .subtitle("Search overlapping audio windows and video frames")
            .active(draft.borrow().audio_video)
            .build();
        types.add(&media);
        let extensions = adw::EntryRow::builder()
            .title("Excluded File Extensions (comma separated)")
            .text(draft.borrow().excluded_extensions.join(", "))
            .build();
        types.add(&extensions);
        page.add(&types);
        page.add(&reader_status_group());
        let integration_group = adw::PreferencesGroup::builder()
            .title("GNOME Integration")
            .build();
        let replace = adw::SwitchRow::builder()
            .title("Use GSeek in GNOME Search")
            .subtitle(
                "Enable GSeek and disable the Files search provider. LocalSearch keeps running.",
            )
            .active(draft.borrow().search_enabled)
            .build();
        integration_group.add(&replace);
        integration_group.add(&scope);
        page.add(&integration_group);
        dialog.add(&page);

        let inference = adw::PreferencesPage::builder()
            .name("inference")
            .title("Inference")
            .icon_name("applications-engineering-symbolic")
            .build();
        let local=adw::PreferencesGroup::builder().title("On This Computer").description("EmbeddingGemma 2 runs privately in a managed helper. The model is downloaded and verified automatically.").build();
        let backends = gtk::StringList::new(&["Automatic", "CPU", "Vulkan", "NVIDIA CUDA"]);
        let backend = adw::ComboRow::builder()
            .title("Compute Backend")
            .model(&backends)
            .selected(match draft.borrow().backend {
                Backend::Auto => 0,
                Backend::Cpu => 1,
                Backend::Vulkan => 2,
                Backend::Cuda => 3,
            })
            .build();
        local.add(&backend);
        let active_backend = adw::ActionRow::builder()
            .title("Active Backend")
            .use_markup(false)
            .build();
        active_backend.set_subtitle_lines(0);
        local.add(&active_backend);
        let detail = adw::ActionRow::builder()
            .title("Inference Status")
            .use_markup(false)
            .build();
        detail.set_subtitle_lines(0);
        let retry = gtk::Button::builder()
            .label("Retry")
            .valign(gtk::Align::Center)
            .build();
        let weak = Rc::downgrade(self);
        retry.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.action("setup", "");
            }
        });
        detail.add_suffix(&retry);
        local.add(&detail);
        let view = BackendView {
            active: active_backend.downgrade(),
            detail: detail.downgrade(),
            retry: retry.downgrade(),
        };
        view.refresh(&self.last_status.borrow());
        self.backend_views.borrow_mut().push(view);
        let weak = Rc::downgrade(self);
        backend.connect_selected_notify(move |row| {
            if let Some(ui) = weak.upgrade() {
                let selected = match row.selected() {
                    1 => Backend::Cpu,
                    2 => Backend::Vulkan,
                    3 => Backend::Cuda,
                    0 => Backend::Auto,
                    _ => return, // An empty/disposed model has no selected backend.
                };
                ui.save_preference(serde_json::json!({"backend": selected}));
            }
        });
        inference.add(&local);
        let external=adw::PreferencesGroup::builder().title("Existing Inference Endpoint").description("Advanced: OpenAI-compatible text embeddings only. Images, scanned PDFs, audio, and video require managed inference. The endpoint must identify an EmbeddingGemma 2 model and return 768 dimensions.").build();
        let use_external = adw::SwitchRow::builder()
            .title("Use an External Endpoint")
            .active(draft.borrow().external.is_some())
            .build();
        external.add(&use_external);
        let address = adw::EntryRow::builder()
            .title("Endpoint URL")
            .text(
                draft
                    .borrow()
                    .external
                    .as_ref()
                    .map(|e| e.url.as_str())
                    .unwrap_or("http://127.0.0.1:8080/v1"),
            )
            .build();
        external.add(&address);
        let model = adw::EntryRow::builder()
            .title("Model Identifier")
            .text(
                draft
                    .borrow()
                    .external
                    .as_ref()
                    .map(|e| e.model.as_str())
                    .unwrap_or("embeddinggemma-2"),
            )
            .build();
        external.add(&model);
        let consent=adw::SwitchRow::builder().title("Allow Sending Content to Another Computer").subtitle("Required for remote HTTPS endpoints. File content and queries will leave this computer.").active(draft.borrow().external.as_ref().is_some_and(|e|e.allow_remote)).build();
        external.add(&consent);
        inference.add(&external);
        dialog.add(&inference);
        let diagnostics = adw::PreferencesPage::builder()
            .title("Index")
            .icon_name("drive-harddisk-symbolic")
            .build();
        let maintenance = adw::PreferencesGroup::builder()
            .title("Index Management")
            .description(
                "Settings and indexed data stay on this computer. Reindexing preserves your files.",
            )
            .build();
        for (title, command) in [
            ("Reindex All Locations", "reindex"),
            ("Retry Failed Files", "retry"),
            ("Retry Model Setup", "setup"),
        ] {
            let row = adw::ActionRow::builder()
                .title(title)
                .activatable(true)
                .build();
            row.add_suffix(&gtk::Image::from_icon_name("view-refresh-symbolic"));
            let weak = Rc::downgrade(self);
            row.connect_activated(move |_| {
                if let Some(ui) = weak.upgrade() {
                    ui.action(command, "");
                }
            });
            maintenance.add(&row);
        }
        let storage = adw::ActionRow::builder()
            .title("Index and Diagnostics")
            .subtitle(glib::markup_escape_text(&self.paths.data.to_string_lossy()))
            .build();
        maintenance.add(&storage);
        diagnostics.add(&maintenance);
        dialog.add(&diagnostics);
        connect_switch(&images, "images", self);
        connect_switch(&media, "audio_video", self);
        connect_switch(&replace, "search_enabled", self);
        extensions.set_show_apply_button(true);
        let weak = Rc::downgrade(self);
        extensions.connect_apply(move |row| {
            if let Some(ui) = weak.upgrade() {
                let values: Vec<_> = row
                    .text()
                    .split(',')
                    .map(|s| s.trim().trim_start_matches('.').to_lowercase())
                    .filter(|s| !s.is_empty())
                    .collect();
                ui.save_preference(serde_json::json!({"excluded_extensions": values}));
            }
        });
        let weak = Rc::downgrade(self);
        let endpoint = use_external.downgrade();
        let endpoint_address = address.downgrade();
        let endpoint_model = model.downgrade();
        let endpoint_consent = consent.downgrade();
        let save_endpoint: Rc<dyn Fn()> = Rc::new(move || {
            let (
                Some(endpoint),
                Some(endpoint_address),
                Some(endpoint_model),
                Some(endpoint_consent),
            ) = (
                endpoint.upgrade(),
                endpoint_address.upgrade(),
                endpoint_model.upgrade(),
                endpoint_consent.upgrade(),
            )
            else {
                return;
            };
            if let Some(ui) = weak.upgrade() {
                let value = if endpoint.is_active() {
                    Some(External {
                        url: endpoint_address.text().to_string(),
                        model: endpoint_model.text().to_string(),
                        allow_remote: endpoint_consent.is_active(),
                    })
                } else {
                    None
                };
                ui.save_preference(serde_json::json!({"external": value}));
            }
        });
        let save = save_endpoint.clone();
        use_external.connect_active_notify(move |_| save());
        let save = save_endpoint.clone();
        consent.connect_active_notify(move |_| save());
        for row in [address, model] {
            row.set_show_apply_button(true);
            let save = save_endpoint.clone();
            row.connect_apply(move |_| save());
        }
        dialog.present(Some(&self.window));
        self.refresh();
    }
}

fn connect_switch(row: &adw::SwitchRow, key: &'static str, ui: &Rc<Window>) {
    let weak = Rc::downgrade(ui);
    row.connect_active_notify(move |row| {
        if let Some(ui) = weak.upgrade() {
            ui.save_preference(serde_json::json!({(key): row.is_active()}));
        }
    });
}
fn save_folders(ui: &Rc<Window>, draft: &Rc<RefCell<Settings>>, excluded: bool) {
    let settings = draft.borrow();
    let (key, folders) = if excluded {
        ("excluded_folders", &settings.excluded_folders)
    } else {
        ("folders", &settings.folders)
    };
    ui.save_preference(serde_json::json!({(key): folders}));
}
fn add_selected_folders(folders: &mut Vec<std::path::PathBuf>, selected: &gio::ListModel) {
    for file in selected.iter::<gio::File>().flatten() {
        if let Some(path) = file.path()
            && !folders.contains(&path)
        {
            folders.push(path);
        }
    }
}

fn reader_status_group() -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder().title("File Readers")
        .description("GSeek uses tools already installed on this computer. Text search needs no extra readers.")
        .build();
    for (title, detail) in crate::extract::reader_status() {
        let row = adw::ActionRow::builder()
            .title(title)
            .subtitle(detail)
            .build();
        row.set_subtitle_lines(0);
        group.add(&row);
    }
    group
}

fn clear_list(list: &gtk::ListBox) {
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
}

fn default_exclusions_group() -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::new();
    let defaults = adw::ExpanderRow::builder()
        .title("Default Exclusions")
        .subtitle("Hidden files, credentials, encryption keys, and generated folders")
        .build();
    for (title, detail) in [
        (
            "Hidden Files and Folders",
            "Names beginning with a dot, including .env, .ssh, and .git".into(),
        ),
        (
            "Security File Extensions",
            crate::config::SENSITIVE_EXTENSIONS
                .iter()
                .map(|s| format!(".{s}"))
                .collect::<Vec<_>>()
                .join(", "),
        ),
        (
            "Credential Filenames",
            crate::config::SENSITIVE_FILENAMES.join(", "),
        ),
        (
            "Generated Folders",
            crate::config::GENERATED_DIRECTORIES.join(", "),
        ),
    ] {
        let row = adw::ActionRow::builder()
            .title(title)
            .subtitle(glib::markup_escape_text(&detail))
            .build();
        defaults.add_row(&row);
    }
    group.add(&defaults);
    let names = adw::ActionRow::builder().title("Standard GNOME File Results")
        .subtitle("Filename matches are included even outside content search folders, including Downloads. Exclusions apply to all results.").build();
    group.add(&names);
    group
}

fn fill_folders(
    list: &gtk::ListBox,
    draft: &Rc<RefCell<Settings>>,
    excluded: bool,
    ui: &Rc<Window>,
) {
    clear_list(list);
    let folders = if excluded {
        draft.borrow().excluded_folders.clone()
    } else {
        draft.borrow().folders.clone()
    };
    for path in folders {
        let row = adw::ActionRow::builder()
            .title(glib::markup_escape_text(
                &draft.borrow().location_label(&path),
            ))
            .build();
        row.set_tooltip_text(Some(&path.to_string_lossy()));
        row.set_title_lines(1);
        row.add_prefix(&gtk::Image::from_icon_name("folder-symbolic"));
        if !excluded && draft.borrow().configured {
            let reindex = gtk::Button::builder()
                .icon_name("view-refresh-symbolic")
                .tooltip_text("Reindex This Location")
                .valign(gtk::Align::Center)
                .build();
            reindex.add_css_class("flat");
            let path = path.clone();
            let weak = Rc::downgrade(ui);
            reindex.connect_clicked(move |_| {
                if let Some(ui) = weak.upgrade() {
                    ui.action("reindex", &path.to_string_lossy());
                }
            });
            row.add_suffix(&reindex);
        }
        let remove = gtk::Button::builder()
            .icon_name("list-remove-symbolic")
            .tooltip_text("Remove Folder")
            .valign(gtk::Align::Center)
            .build();
        remove.add_css_class("flat");
        row.add_suffix(&remove);
        let draft = draft.clone();
        let refresh_list = list.clone();
        let weak = Rc::downgrade(ui);
        remove.connect_clicked(move |_| {
            let mut s = draft.borrow_mut();
            let folders = if excluded {
                &mut s.excluded_folders
            } else {
                &mut s.folders
            };
            folders.retain(|p| p != &path);
            drop(s);
            if let Some(ui) = weak.upgrade() {
                save_folders(&ui, &draft, excluded);
                fill_folders(&refresh_list, &draft, excluded, &ui);
            }
        });
        list.append(&row);
    }
}
fn connect_folder_picker(
    button: &gtk::Button,
    parent: &adw::ApplicationWindow,
    list: &gtk::ListBox,
    draft: &Rc<RefCell<Settings>>,
    excluded: bool,
    ui: &Rc<Window>,
) {
    let parent = parent.clone();
    let list = list.clone();
    let draft = draft.clone();
    let weak = Rc::downgrade(ui);
    button.connect_clicked(move |_| {
        let picker = gtk::FileDialog::builder()
            .title(if excluded {
                "Exclude Folders"
            } else {
                "Choose Search Folders"
            })
            .modal(true)
            .build();
        let list = list.clone();
        let draft = draft.clone();
        let weak = weak.clone();
        picker.select_multiple_folders(Some(&parent), gio::Cancellable::NONE, move |result| {
            if let Ok(files) = result {
                let mut s = draft.borrow_mut();
                let folders = if excluded {
                    &mut s.excluded_folders
                } else {
                    &mut s.folders
                };
                add_selected_folders(folders, &files);
                drop(s);
                if let Some(ui) = weak.upgrade() {
                    save_folders(&ui, &draft, excluded);
                    fill_folders(&list, &draft, excluded, &ui);
                }
            }
        });
    });
}

fn spawn<T: Send + 'static>(
    work: impl FnOnce() -> Result<T> + Send + 'static,
    done: impl FnOnce(Result<T>) + 'static,
) {
    let (tx, rx) = async_channel::bounded(1);
    std::thread::spawn(move || {
        let _ = tx.send_blocking(work());
    });
    glib::spawn_future_local(async move {
        if let Ok(result) = rx.recv().await {
            done(result);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        path::{Path, PathBuf},
        time::Instant,
    };

    #[test]
    fn folder_selection_adds_the_whole_batch_without_duplicates() {
        let selected = gio::ListStore::new::<gio::File>();
        selected.append(&gio::File::for_path("/home/test/Documents"));
        selected.append(&gio::File::for_path("/home/test/Pictures"));
        selected.append(&gio::File::for_path("/home/test/Documents"));
        selected.append(&gio::File::for_uri("https://example.com/nonlocal"));
        for mut folders in [vec![], vec![PathBuf::from("/home/test/Documents")]] {
            add_selected_folders(&mut folders, selected.upcast_ref());
            assert_eq!(
                folders,
                vec![
                    PathBuf::from("/home/test/Documents"),
                    PathBuf::from("/home/test/Pictures")
                ]
            );
        }
    }
    fn backend_row(widget: &gtk::Widget) -> Option<adw::ComboRow> {
        if let Some(row) = widget.downcast_ref::<adw::ComboRow>()
            && row.title() == "Compute Backend"
        {
            return Some(row.clone());
        }
        let mut child = widget.first_child();
        while let Some(widget) = child {
            if let Some(row) = backend_row(&widget) {
                return Some(row);
            }
            child = widget.next_sibling();
        }
        None
    }

    fn until(mut predicate: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(45);
        loop {
            while glib::MainContext::default().pending() {
                glib::MainContext::default().iteration(false);
            }
            if predicate() {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "GTK integration condition timed out"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    fn results_list(widget: &gtk::Widget) -> Option<gtk::ListView> {
        if let Some(list) = widget.downcast_ref::<gtk::ListView>() {
            return Some(list.clone());
        }
        let mut child = widget.first_child();
        while let Some(widget) = child {
            if let Some(list) = results_list(&widget) {
                return Some(list);
            }
            child = widget.next_sibling();
        }
        None
    }
    fn capture(window: &adw::ApplicationWindow, path: &Path) {
        let paintable = gtk::WidgetPaintable::new(Some(window));
        let mut node = None;
        until(|| {
            let snapshot = gtk::Snapshot::new();
            paintable.snapshot(&snapshot, window.width() as f64, window.height() as f64);
            node = snapshot.to_node();
            if node.is_none() {
                window.queue_draw();
            }
            node.is_some()
        });
        let node = node.unwrap();
        let renderer = window.renderer().expect("Window has a renderer");
        renderer
            .render_texture(&node, None)
            .save_to_png(path)
            .unwrap();
    }
    fn close_dialog(window: &adw::ApplicationWindow) {
        let dialog = window.visible_dialog().unwrap();
        let closed = Rc::new(Cell::new(false));
        let flag = closed.clone();
        dialog.connect_closed(move |_| flag.set(true));
        dialog.close();
        until(|| closed.get());
        // Complete the parent allocation after the dialog leaves its host.
        let frame = Instant::now();
        until(|| frame.elapsed() > Duration::from_millis(300));
    }
    #[test]
    #[ignore = "Needs a display and the real inference fixture; run tests/run-e2e.sh with GSEEK_GUI_TEST_COMMAND"]
    fn native_views() {
        adw::init().unwrap();
        let app = adw::Application::builder()
            .application_id("io.github.FarisZR.GSeek.UITest")
            .flags(gio::ApplicationFlags::NON_UNIQUE)
            .build();
        app.register(gio::Cancellable::NONE).unwrap();
        // The desktop and D-Bus activation environments can have different XDG
        // directories. Preferences must show what the provider actually saved.
        let local_config = tempfile::tempdir().unwrap();
        let mut ui_paths = Paths::from_env();
        ui_paths.config = local_config.path().join("gseek");
        ui_paths
            .save(&Settings {
                configured: true,
                ..Settings::default()
            })
            .unwrap();
        let ui = Window::build(&app, ui_paths);
        let theme = gtk::IconTheme::new();
        theme.set_search_path(&[]);
        theme.add_resource_path("/io/github/FarisZR/GSeek/icons");
        assert!(
            theme.has_icon(APP_ID),
            "App icon must work with no installed icon theme"
        );
        let icon = theme.lookup_icon(
            APP_ID,
            &[],
            128,
            1,
            gtk::TextDirection::None,
            gtk::IconLookupFlags::empty(),
        );
        assert!(icon.file().unwrap().uri().starts_with("resource:///"));
        // Control icons must also survive an absent/stale desktop icon theme.
        for file in gio::resources_enumerate_children(
            "/io/github/FarisZR/GSeek/icons/scalable/actions",
            gio::ResourceLookupFlags::NONE,
        )
        .unwrap()
        {
            let name = file.strip_suffix(".svg").unwrap();
            assert!(theme.has_icon(name), "Missing control icon: {name}");
            let icon = theme.lookup_icon(
                name,
                &[],
                16,
                1,
                gtk::TextDirection::None,
                gtk::IconLookupFlags::empty(),
            );
            assert!(
                icon.file().unwrap().uri().starts_with("resource:///"),
                "{name}"
            );
        }
        let texture = gtk::gdk::Texture::from_resource(
            "/io/github/FarisZR/GSeek/icons/512x512/apps/io.github.FarisZR.GSeek.png",
        );
        assert_eq!((texture.width(), texture.height()), (512, 512));
        let output = std::env::var_os("GSEEK_SCREENSHOT_DIR").map(PathBuf::from);
        if let Some(dir) = &output {
            std::fs::create_dir_all(dir).unwrap();
        }
        ui.window.present();
        until(|| ui.window.is_mapped());
        let frame = Instant::now();
        until(|| frame.elapsed() > Duration::from_millis(300));
        if let Some(dir) = &output {
            capture(&ui.window, &dir.join("welcome.png"));
        }
        ui.refresh();
        until(|| ui.main_stack.visible_child_name().as_deref() == Some("library"));
        until(|| ui.logs.first_child().is_some());
        // Allow a full frame before recording the native widget scene.
        let frame = Instant::now();
        until(|| frame.elapsed() > Duration::from_millis(300));
        if let Some(dir) = &output {
            capture(&ui.window, &dir.join("activity.png"));
        }
        ui.preferences(None);
        until(|| ui.window.visible_dialog().is_some());
        {
            let dialog = ui.window.visible_dialog().unwrap();
            let row = backend_row(dialog.upcast_ref()).unwrap();
            let saved = service::read_settings().unwrap();
            assert_eq!(
                row.selected(),
                match saved.backend {
                    Backend::Auto => 0,
                    Backend::Cpu => 1,
                    Backend::Vulkan => 2,
                    Backend::Cuda => 3,
                },
                "Preferences must use the background service’s saved choice"
            );
        }
        let frame = Instant::now();
        until(|| frame.elapsed() > Duration::from_millis(400));
        if let Some(dir) = &output {
            capture(&ui.window, &dir.join("preferences.png"));
        }
        if service::read_settings().unwrap().backend == Backend::Cpu {
            {
                let dialog = ui.window.visible_dialog().unwrap();
                let backend = backend_row(dialog.upcast_ref()).unwrap();
                backend.set_selected(0);
                backend.set_selected(1);
                backend.set_selected(0);
            }
            // Close without a separate save action, then reopen and verify persistence.
            close_dialog(&ui.window);
            // Reopen immediately, while saves may still be in flight.
            ui.preferences(None);
            until(|| ui.window.visible_dialog().is_some());
            {
                let dialog = ui.window.visible_dialog().unwrap();
                let backend = backend_row(dialog.upcast_ref()).unwrap();
                assert_eq!(backend.selected(), 0);
                backend.set_selected(3);
            }
            close_dialog(&ui.window);
            ui.preferences(None);
            until(|| ui.window.visible_dialog().is_some());
            {
                let dialog = ui.window.visible_dialog().unwrap();
                let backend = backend_row(dialog.upcast_ref()).unwrap();
                assert_eq!(
                    backend.selected(),
                    3,
                    "CUDA must survive closing and reopening"
                );
                backend.set_model(None::<&gio::ListModel>);
            }
            until(|| service::read_status().is_ok_and(|s| s.state == "error"));
            assert_eq!(service::read_settings().unwrap().backend, Backend::Cuda);
            until(|| {
                ui.refresh();
                ui.backend.text() == "Backend: Not running · NVIDIA CUDA selected"
            });
            {
                let views = ui.backend_views.borrow();
                let view = views.last().unwrap();
                assert_eq!(
                    view.active.upgrade().unwrap().subtitle().as_deref(),
                    Some("Not running · NVIDIA CUDA selected")
                );
                assert!(
                    view.detail
                        .upgrade()
                        .unwrap()
                        .subtitle()
                        .unwrap()
                        .contains("CUDA test driver unavailable")
                );
                assert!(view.retry.upgrade().unwrap().is_visible());
            }
            close_dialog(&ui.window);
            ui.preferences(None);
            until(|| ui.window.visible_dialog().is_some());
            {
                let dialog = ui.window.visible_dialog().unwrap();
                let backend = backend_row(dialog.upcast_ref()).unwrap();
                assert_eq!(backend.selected(), 3);
                backend.set_selected(0);
            }
            close_dialog(&ui.window);
            ui.preferences(None);
            until(|| ui.window.visible_dialog().is_some());
            assert_eq!(
                backend_row(ui.window.visible_dialog().unwrap().upcast_ref())
                    .unwrap()
                    .selected(),
                0
            );
            until(|| {
                service::read_status()
                    .is_ok_and(|s| s.ready && s.requested_backend == Backend::Auto)
            });
            until(|| {
                ui.refresh();
                ui.backend.text() == "Backend: Automatic · CPU"
            });
            let dialog = ui
                .window
                .visible_dialog()
                .unwrap()
                .downcast::<adw::PreferencesDialog>()
                .unwrap();
            dialog.set_visible_page_name("inference");
            let frame = Instant::now();
            until(|| frame.elapsed() > Duration::from_millis(400));
            if let Some(dir) = &output {
                capture(&ui.window, &dir.join("inference.png"));
            }
            backend_row(dialog.upcast_ref()).unwrap().set_selected(1);
            until(|| service::read_settings().unwrap().backend == Backend::Cpu);
            until(|| {
                service::read_status().is_ok_and(|s| {
                    s.ready && s.requested_backend == Backend::Cpu && s.backend == "CPU"
                })
            });
            until(|| {
                ui.refresh();
                ui.backend.text() == "Backend: CPU"
            });
        }
        close_dialog(&ui.window);
        ui.show_results("Show All Results");
        until(|| ui.search_results.n_items() >= 7);
        assert_eq!(
            (0..ui.search_results.n_items())
                .filter(|i| {
                    ui.search_results
                        .item(*i)
                        .unwrap()
                        .downcast::<glib::BoxedAnyObject>()
                        .unwrap()
                        .borrow::<crate::store::SearchHit>()
                        .name
                        .starts_with("Show All Results")
                })
                .count(),
            7
        );
        assert_eq!(
            ui.search_content.visible_child_name().as_deref(),
            Some("results")
        );
        let frame = Instant::now();
        until(|| frame.elapsed() > Duration::from_millis(300));
        if let Some(dir) = &output {
            capture(&ui.window, &dir.join("results.png"));
        }
        let opened =
            PathBuf::from(std::env::var_os("GSEEK_TEST_ROOT").unwrap()).join("opened.json");
        let _ = std::fs::remove_file(&opened);
        let target = ui
            .search_results
            .item(0)
            .unwrap()
            .downcast::<glib::BoxedAnyObject>()
            .unwrap()
            .borrow::<crate::store::SearchHit>()
            .uri
            .clone();
        let started = Instant::now();
        results_list(ui.window.upcast_ref())
            .unwrap()
            .emit_by_name::<()>("activate", &[&0_u32]);
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "File validation blocked GTK"
        );
        let frame = Instant::now();
        until(|| frame.elapsed() > Duration::from_millis(250));
        assert!(
            frame.elapsed() < Duration::from_secs(1),
            "GTK stopped dispatching while opening a file"
        );
        until(|| opened.exists());
        let arguments: Vec<String> =
            serde_json::from_slice(&std::fs::read(opened).unwrap()).unwrap();
        let expected = gio::File::for_uri(&target);
        assert!(
            arguments
                .iter()
                .any(|arg| gio::File::for_commandline_arg(arg).equal(&expected))
        );
        ui.show_results("Rental Agreement");
        until(|| ui.search_results.n_items() > 0 && ui.search_generation.get() > 1);
        let item = ui
            .search_results
            .item(0)
            .unwrap()
            .downcast::<glib::BoxedAnyObject>()
            .unwrap();
        assert_eq!(
            item.borrow::<crate::store::SearchHit>().name,
            "Rental Agreement.txt"
        );
        if service::read_settings().unwrap().backend == Backend::Cpu {
            ui.show_results("Show All Results");
            until(|| ui.search_results.n_items() >= 7);
            let previous = service::read_settings().unwrap().excluded_folders;
            let mut excluded = previous.clone();
            let downloads = std::path::PathBuf::from(std::env::var_os("GSEEK_TEST_ROOT").unwrap())
                .join("Downloads");
            excluded.push(downloads);
            let weak = Rc::downgrade(&ui);
            let timer = glib::timeout_add_local(Duration::from_millis(100), move || {
                if let Some(ui) = weak.upgrade() {
                    ui.refresh();
                    glib::ControlFlow::Continue
                } else {
                    glib::ControlFlow::Break
                }
            });
            let generation = ui.search_generation.get();
            ui.save_preference(serde_json::json!({"excluded_folders":excluded}));
            until(|| {
                !ui.settings_busy.get()
                    && !ui.search_refresh_pending.get()
                    && ui.search_generation.get() > generation
                    && (ui.search_results.n_items() > 0
                        || ui.search_placeholder.title() == "No Results")
            });
            assert!((0..ui.search_results.n_items()).all(|i| {
                !ui.search_results
                    .item(i)
                    .unwrap()
                    .downcast::<glib::BoxedAnyObject>()
                    .unwrap()
                    .borrow::<crate::store::SearchHit>()
                    .name
                    .starts_with("Show All Results")
            }));
            ui.save_preference(serde_json::json!({"excluded_folders":previous}));
            until(|| {
                ui.search_results.n_items() >= 7
                    && !ui.settings_busy.get()
                    && !ui.search_refresh_pending.get()
            });
            timer.remove();
        }
        ui.show_results("");
        until(|| ui.search_results.n_items() == 0);
        ui.setup();
        until(|| ui.main_stack.visible_child_name().as_deref() == Some("setup"));
        assert!(ui.window.visible_dialog().is_none());
        assert_eq!(ui.main_stack.visible_child_name().as_deref(), Some("setup"));
        let frame = Instant::now();
        until(|| frame.elapsed() > Duration::from_millis(400));
        if let Some(dir) = &output {
            capture(&ui.window, &dir.join("setup.png"));
        }
        ui.window.set_default_size(390, 700);
        until(|| ui.window.width() <= 400);
        let frame = Instant::now();
        until(|| frame.elapsed() > Duration::from_millis(300));
        if let Some(dir) = &output {
            capture(&ui.window, &dir.join("setup-narrow.png"));
        }
        ui.main_stack.set_visible_child_name("welcome");
        let frame = Instant::now();
        until(|| frame.elapsed() > Duration::from_millis(300));
        if let Some(dir) = &output {
            capture(&ui.window, &dir.join("welcome-narrow.png"));
        }
        ui.main_stack.set_visible_child_name("library");
        let frame = Instant::now();
        until(|| frame.elapsed() > Duration::from_millis(300));
        if let Some(dir) = &output {
            capture(&ui.window, &dir.join("activity-narrow.png"));
        }
        ui.window.close();
    }
}
