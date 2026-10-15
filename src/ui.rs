use crate::{
    APP_ID,
    config::{Backend, External, Paths, Settings},
    integration, service,
    store::SearchHit,
};
use adw::prelude::*;
use anyhow::Result;
use glib::variant::ToVariant;
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::Duration,
};

struct Window {
    window: adw::ApplicationWindow,
    entry: gtk::SearchEntry,
    results: gtk::ListBox,
    result_state: adw::StatusPage,
    result_stack: gtk::Stack,
    main_stack: gtk::Stack,
    view_stack: adw::ViewStack,
    banner: adw::Banner,
    status_title: gtk::Label,
    status_detail: gtk::Label,
    counts: gtk::Label,
    progress: gtk::ProgressBar,
    current: gtk::Label,
    logs: gtk::ListBox,
    pause: gtk::Button,
    generation: Cell<u64>,
    refreshing: Cell<bool>,
    paths: Paths,
}

pub fn run(paths: Paths, query: Option<String>) -> Result<()> {
    let app = adw::Application::builder()
        .application_id(APP_ID)
        .flags(gio::ApplicationFlags::HANDLES_COMMAND_LINE)
        .build();
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
    app.connect_command_line(move |app, command| {
        app.activate();
        let args = command.arguments();
        if let Some(ui) = state.borrow().as_ref()
            && let Some(query) = args.get(1).and_then(|s| s.to_str())
        {
            ui.entry.set_text(query);
        }
        glib::ExitCode::SUCCESS
    });
    let exit = app.run_with_args(&["gseek", query.as_deref().unwrap_or("")]);
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
        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title("GSeek")
            .default_width(900)
            .default_height(700)
            .build();
        let root = adw::ToolbarView::new();
        let header = adw::HeaderBar::new();
        let menu = gio::Menu::new();
        menu.append(Some("Preferences"), Some("app.preferences"));
        menu.append(Some("About GSeek"), Some("app.about"));
        let menu_button = gtk::MenuButton::builder()
            .icon_name("open-menu-symbolic")
            .menu_model(&menu)
            .tooltip_text("Main Menu")
            .build();
        header.pack_end(&menu_button);
        root.add_top_bar(&header);
        let main_stack = gtk::Stack::new();
        root.set_content(Some(&main_stack));
        window.set_content(Some(&root));

        let welcome=adw::StatusPage::builder().icon_name(APP_ID).title("Find your files by what you remember")
            .description("Describe a document, photo, or moment in a recording. GSeek brings private, multilingual search to your GNOME overview.").build();
        let welcome_box = vertical(16);
        let privacy = label(
            "Everything stays on this computer.\nEmbeddingGemma 2 downloads once (865 MB).",
            "dim-label",
        );
        welcome_box.append(&privacy);
        let start = gtk::Button::with_label("Set Up Search");
        start.add_css_class("suggested-action");
        start.add_css_class("pill");
        start.set_halign(gtk::Align::Center);
        welcome_box.append(&start);
        welcome.set_child(Some(&welcome_box));
        main_stack.add_named(&welcome, Some("welcome"));

        let body = vertical(0);
        let banner = adw::Banner::builder()
            .title("Setting up semantic search")
            .button_label("View Activity")
            .revealed(false)
            .build();
        body.append(&banner);
        let view_stack = adw::ViewStack::new();
        view_stack.set_vexpand(true);
        body.append(&view_stack);
        main_stack.add_named(&body, Some("library"));
        let switcher = adw::ViewSwitcher::builder()
            .stack(&view_stack)
            .policy(adw::ViewSwitcherPolicy::Wide)
            .build();
        header.set_title_widget(Some(&switcher));
        let switcher_bar = adw::ViewSwitcherBar::builder()
            .stack(&view_stack)
            .reveal(true)
            .build();
        root.add_bottom_bar(&switcher_bar);
        // Let the title switcher collapse naturally on narrow windows, keeping the same destinations below.
        let breakpoint =
            adw::Breakpoint::new(adw::BreakpointCondition::parse("max-width: 600px").unwrap());
        breakpoint.add_setter(&switcher, "visible", Some(&false.to_value()));
        breakpoint.add_setter(&switcher_bar, "reveal", Some(&true.to_value()));
        window.add_breakpoint(breakpoint);
        switcher_bar.set_reveal(false);

        let search_box = vertical(16);
        margins(&search_box, 24);
        let entry = gtk::SearchEntry::builder()
            .placeholder_text("Describe what you’re looking for…")
            .hexpand(true)
            .build();
        entry.set_tooltip_text(Some(
            "Search filenames, exact words, and meaning in any language",
        ));
        let clamp = adw::Clamp::builder()
            .maximum_size(760)
            .child(&entry)
            .build();
        search_box.append(&clamp);
        let result_stack = gtk::Stack::new();
        result_stack.set_vexpand(true);
        let result_state=adw::StatusPage::builder().icon_name("system-search-symbolic").title("What are you looking for?")
            .description("Try “the clause about pets”, “a photo of the beach”,\nor describe a file in English, German, or Arabic.").build();
        result_stack.add_named(&result_state, Some("message"));
        let results = gtk::ListBox::new();
        results.set_valign(gtk::Align::Start);
        results.add_css_class("boxed-list");
        results.set_selection_mode(gtk::SelectionMode::Single);
        let scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&results)
            .build();
        let result_clamp = adw::Clamp::builder()
            .maximum_size(800)
            .child(&scroll)
            .build();
        result_stack.add_named(&result_clamp, Some("results"));
        search_box.append(&result_stack);
        let hint = label("Search here or press Super and start typing.", "dim-label");
        search_box.append(&hint);
        view_stack.add_titled_with_icon(
            &search_box,
            Some("search"),
            "Search",
            "system-search-symbolic",
        );

        let activity_box = vertical(20);
        margins(&activity_box, 24);
        let status_title = label("Preparing search", "title-1");
        status_title.set_halign(gtk::Align::Start);
        activity_box.append(&status_title);
        let status_detail = label("", "dim-label");
        status_detail.set_xalign(0.0);
        status_detail.set_selectable(true);
        activity_box.append(&status_detail);
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
        let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let pause = gtk::Button::with_label("Pause Indexing");
        buttons.append(&pause);
        let scan = gtk::Button::with_label("Index Now");
        buttons.append(&scan);
        let retry = gtk::Button::with_label("Retry Failed");
        buttons.append(&retry);
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
        view_stack.add_titled_with_icon(
            &activity_clamp,
            Some("activity"),
            "Indexing & Activity",
            "view-list-symbolic",
        );
        let ui = Rc::new(Self {
            window,
            entry,
            results,
            result_state,
            result_stack,
            main_stack,
            view_stack,
            banner,
            status_title,
            status_detail,
            counts,
            progress,
            current: current_file,
            logs,
            pause,
            generation: Cell::new(0),
            refreshing: Cell::new(false),
            paths,
        });

        let weak = Rc::downgrade(&ui);
        start.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.setup();
            }
        });
        let weak = Rc::downgrade(&ui);
        ui.banner.connect_button_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.view_stack.set_visible_child_name("activity");
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
        let weak = Rc::downgrade(&ui);
        ui.entry.connect_search_changed(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.start_search();
            }
        });
        let weak = Rc::downgrade(&ui);
        ui.entry.connect_activate(move |_| {
            if let Some(ui) = weak.upgrade()
                && let Some(row) = ui.results.row_at_index(0)
            {
                ui.results.emit_by_name::<()>("row-activated", &[&row]);
            }
        });
        let weak = Rc::downgrade(&ui);
        ui.results.connect_row_activated(move |_, row| {
            if let Some(ui) = weak.upgrade()
                && let Some(uri) = row.tooltip_text()
                && let Err(e) = integration::open_result(&ui.paths, &uri)
            {
                ui.error(&format!("{e:#}"));
            }
        });
        let preferences = gio::SimpleAction::new("preferences", None);
        let weak = Rc::downgrade(&ui);
        preferences.connect_activate(move |_, _| {
            if let Some(ui) = weak.upgrade() {
                ui.preferences(None);
            }
        });
        app.add_action(&preferences);
        let about = gio::SimpleAction::new("about", None);
        let weak = Rc::downgrade(&ui);
        about.connect_activate(move |_,_|{if let Some(ui)=weak.upgrade(){
            let dialog=adw::AboutDialog::builder().application_name("GSeek").application_icon(APP_ID).version(env!("CARGO_PKG_VERSION"))
                .developer_name("FarisZR").website("https://github.com/FarisZR/GSeek").issue_url("https://github.com/FarisZR/GSeek/issues")
                .license_type(gtk::License::Gpl30).comments("Private, multilingual file search for GNOME. Powered by EmbeddingGemma 2 and llama.cpp.").build();dialog.present(Some(&ui.window));
        }});
        app.add_action(&about);
        app.set_accels_for_action("app.preferences", &["<Primary>comma"]);
        let focus = gio::SimpleAction::new("focus-search", None);
        let weak = Rc::downgrade(&ui);
        focus.connect_activate(move |_, _| {
            if let Some(ui) = weak.upgrade() {
                ui.view_stack.set_visible_child_name("search");
                ui.entry.grab_focus();
            }
        });
        app.add_action(&focus);
        app.set_accels_for_action("app.focus-search", &["<Primary>f"]);
        ui
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
        spawn(service::read_status, move |result| {
            if let Some(ui) = weak.upgrade() {
                ui.refreshing.set(false);
                match result {
                    Ok(s) => {
                        let configured = ui.paths.load().map(|s| s.configured).unwrap_or(false);
                        ui.main_stack.set_visible_child_name(
                            if configured || s.state == "setup" || s.state == "error" {
                                "library"
                            } else {
                                "welcome"
                            },
                        );
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
                        ui.pause.set_label(if s.paused {
                            "Resume Indexing"
                        } else {
                            "Pause Indexing"
                        });
                    }
                    Err(e) => {
                        ui.banner.set_revealed(true);
                        ui.banner.set_title("Cannot connect to background search. Open Preferences to repair setup.");
                        ui.status_detail.set_text(&format!("{e:#}"));
                    }
                }
            }
        });
        if self.view_stack.visible_child_name().as_deref() == Some("activity") {
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
                            "emblem-ok-symbolic"
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

    fn start_search(self: &Rc<Self>) {
        let query = self.entry.text().to_string();
        let generation = self.generation.get() + 1;
        self.generation.set(generation);
        if query.trim().is_empty() {
            self.result_state.set_title("What are you looking for?");
            self.result_state
                .set_description(Some("Describe a file in your own words, or type its name."));
            self.result_stack.set_visible_child_name("message");
            return;
        }
        self.result_state.set_title("Searching…");
        self.result_state
            .set_description(Some("Matching your words with your files."));
        self.result_stack.set_visible_child_name("message");
        let weak = Rc::downgrade(self);
        spawn(
            move || service::search(&query, 100),
            move |result| {
                if let Some(ui) = weak.upgrade() {
                    if ui.generation.get() != generation {
                        return;
                    }
                    match result {
                        Ok(hits) if !hits.is_empty() => {
                            clear_list(&ui.results);
                            for hit in hits {
                                ui.append_hit(&hit);
                            }
                            ui.result_stack.set_visible_child_name("results");
                        }
                        Ok(_) => {
                            ui.result_state.set_title("No matching files");
                            ui.result_state.set_description(Some("Try another description, or check your search locations in Preferences."));
                            ui.result_stack.set_visible_child_name("message");
                        }
                        Err(e) => {
                            ui.result_state.set_title("Semantic search is unavailable");
                            ui.result_state.set_description(Some(&glib::markup_escape_text(&format!(
                                "{e:#}\n\nOpen Indexing & Activity or retry setup in Preferences."
                            ))));
                            ui.result_stack.set_visible_child_name("message");
                        }
                    }
                }
            },
        );
    }

    fn append_hit(&self, hit: &SearchHit) {
        let location = self
            .paths
            .load()
            .map(|s| s.location_label(hit.path.parent().unwrap_or(&hit.path)))
            .unwrap_or_else(|_| hit.path.to_string_lossy().into_owned());
        let suffix = if let Some(page) = hit.page {
            format!(" · Page {page}")
        } else if let Some(s) = hit.seconds {
            format!(" · {}:{:02}", s as u64 / 60, s as u64 % 60)
        } else {
            String::new()
        };
        let detail = if hit.snippet.trim().is_empty() {
            format!("{location}{suffix}")
        } else {
            format!(
                "{}\n{location}{suffix}",
                hit.snippet.lines().collect::<Vec<_>>().join(" ")
            )
        };
        let row = adw::ActionRow::builder()
            .title(glib::markup_escape_text(&hit.name))
            .subtitle(glib::markup_escape_text(&detail))
            .activatable(true)
            .build();
        row.set_title_lines(1);
        row.set_subtitle_lines(2);
        row.set_tooltip_text(Some(&hit.uri));
        let image = gtk::Image::from_gicon(&integration::icon_for(&hit.path, &hit.mime));
        image.set_pixel_size(40);
        row.add_prefix(&image);
        row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
        self.results.append(&row);
    }

    fn apply_settings(
        self: &Rc<Self>,
        settings: Settings,
        dialog: &adw::Dialog,
        button: &gtk::Button,
    ) {
        if settings.folders.is_empty() {
            self.error("Choose at least one search folder.");
            return;
        }
        if let Err(error) = settings.validate() {
            self.error(&format!("{error:#}"));
            return;
        }
        button.set_sensitive(false);
        let paths = self.paths.clone();
        let weak = Rc::downgrade(self);
        let close = dialog.downgrade();
        let button = button.clone();
        spawn(
            move || {
                integration::install(&paths, &service::launcher()?)?;
                service::call(
                    "Configure",
                    Some(&(serde_json::to_string(&settings)?,).to_variant()),
                )?;
                Ok(())
            },
            move |result| {
                button.set_sensitive(true);
                if let Some(ui) = weak.upgrade() {
                    match result {
                        Ok(()) => {
                            if let Some(dialog) = close.upgrade() {
                                dialog.close();
                            }
                            ui.main_stack.set_visible_child_name("library");
                            ui.view_stack.set_visible_child_name("activity");
                            ui.refresh();
                        }
                        Err(error) => ui.error(&format!("{error:#}")),
                    }
                }
            },
        );
    }

    fn setup(self: &Rc<Self>) {
        let settings = match self.paths.load() {
            Ok(s) => s,
            Err(e) => {
                self.error(&format!("{e:#}"));
                return;
            }
        };
        let draft = Rc::new(RefCell::new(settings));
        let dialog = adw::Dialog::builder()
            .title("Set Up GSeek")
            .content_width(600)
            .content_height(580)
            .build();
        let toolbar = adw::ToolbarView::new();
        let header = adw::HeaderBar::new();
        toolbar.add_top_bar(&header);
        let content = vertical(24);
        margins(&content, 24);
        let intro = label("Your files. Your words.", "title-1");
        intro.set_xalign(0.0);
        content.append(&intro);
        let description = label(
            "Choose the folders you want to search. GSeek downloads EmbeddingGemma 2 once (865 MB) and keeps your search library on this computer.",
            "dim-label",
        );
        description.set_xalign(0.0);
        content.append(&description);
        let locations = adw::PreferencesGroup::builder()
            .title("Search Locations")
            .description("Files must also be included in GNOME’s Search Locations.")
            .build();
        let folders = gtk::ListBox::new();
        folders.add_css_class("boxed-list");
        folders.set_selection_mode(gtk::SelectionMode::None);
        fill_folders(&folders, &draft, false, self);
        locations.add(&folders);
        let add = gtk::Button::with_label("Add Folder…");
        add.set_valign(gtk::Align::Center);
        locations.set_header_suffix(Some(&add));
        connect_folder_picker(&add, &self.window, &folders, &draft, false, self);
        content.append(&locations);
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
        toolbar.set_content(Some(&scroller));
        let footer = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        margins(&footer, 16);
        let advanced = gtk::Button::with_label("Advanced Setup…");
        advanced.add_css_class("flat");
        footer.append(&advanced);
        let start = gtk::Button::with_label("Download & Start");
        start.add_css_class("suggested-action");
        start.set_hexpand(true);
        footer.append(&start);
        toolbar.add_bottom_bar(&footer);
        dialog.set_child(Some(&toolbar));
        let weak = Rc::downgrade(self);
        let close = dialog.downgrade();
        let settings = draft.clone();
        start.connect_clicked(move |button| {
            if let Some(ui) = weak.upgrade()
                && let Some(dialog) = close.upgrade()
            {
                let mut s = settings.borrow().clone();
                s.search_enabled = replace.is_active();
                ui.apply_settings(s, &dialog, button);
            }
        });
        let weak = Rc::downgrade(self);
        let close = dialog.downgrade();
        advanced.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                if let Some(dialog) = close.upgrade() {
                    dialog.close();
                }
                ui.preferences(Some(draft.borrow().clone()));
            }
        });
        dialog.present(Some(&self.window));
    }

    fn preferences(self: &Rc<Self>, initial: Option<Settings>) {
        let settings = match initial.map(Ok).unwrap_or_else(|| self.paths.load()) {
            Ok(s) => s,
            Err(e) => {
                self.error(&format!("{e:#}"));
                return;
            }
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
        let locations=adw::PreferencesGroup::builder().title("Search Locations").description("GSeek uses GNOME LocalSearch. Only files in both these folders and GNOME’s search locations are discovered. Hidden folders, credentials, caches, and build outputs are excluded.").build();
        let folder_list = gtk::ListBox::new();
        folder_list.add_css_class("boxed-list");
        folder_list.set_selection_mode(gtk::SelectionMode::None);
        fill_folders(&folder_list, &draft, false, self);
        locations.add(&folder_list);
        let add = gtk::Button::with_label("Add Folder…");
        add.set_valign(gtk::Align::Center);
        locations.set_header_suffix(Some(&add));
        connect_folder_picker(&add, &self.window, &folder_list, &draft, false, self);
        let scope = adw::ActionRow::builder()
            .title("GNOME Search Locations")
            .subtitle("Open Settings if a selected folder does not appear in the index")
            .activatable(true)
            .build();
        scope.add_suffix(&gtk::Image::from_icon_name("external-link-symbolic"));
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
        let exclude = gtk::Button::with_label("Exclude Folder…");
        exclude.set_valign(gtk::Align::Center);
        exclusions.set_header_suffix(Some(&exclude));
        connect_folder_picker(&exclude, &self.window, &exclude_list, &draft, true, self);
        page.add(&exclusions);
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
        let apply_group = adw::PreferencesGroup::new();
        let apply = gtk::Button::with_label("Apply Changes");
        apply.add_css_class("suggested-action");
        apply_group.add(&apply);
        page.add(&apply_group);
        dialog.add(&page);

        let inference = adw::PreferencesPage::builder()
            .title("Inference")
            .icon_name("applications-engineering-symbolic")
            .build();
        let local=adw::PreferencesGroup::builder().title("On This Computer").description("EmbeddingGemma 2 runs privately in a managed helper. The model is downloaded and verified automatically.").build();
        let backends = gtk::StringList::new(&[
            "Automatic (recommended)",
            "CPU",
            "Vulkan (AMD / Intel)",
            "NVIDIA CUDA",
            "AMD ROCm (advanced)",
        ]);
        let backend = adw::ComboRow::builder()
            .title("Compute Backend")
            .model(&backends)
            .selected(match draft.borrow().backend {
                Backend::Auto => 0,
                Backend::Cpu => 1,
                Backend::Vulkan => 2,
                Backend::Cuda => 3,
                Backend::Rocm => 4,
            })
            .build();
        local.add(&backend);
        let overrides = gtk::StringList::new(&[
            "None",
            "10.3.0 (RX 6700 XT / gfx1031)",
            "11.0.0",
            "11.0.1",
            "12.0.0",
        ]);
        let rocm = adw::ComboRow::builder()
            .title("ROCm Compatibility Override")
            .subtitle("Used only for ROCm; Vulkan does not need this override")
            .model(&overrides)
            .selected(match draft.borrow().rocm_override.as_deref() {
                Some("10.3.0") => 1,
                Some("11.0.0") => 2,
                Some("11.0.1") => 3,
                Some("12.0.0") => 4,
                _ => 0,
            })
            .build();
        local.add(&rocm);
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
        let weak = Rc::downgrade(self);
        let close = dialog.downgrade();
        apply.connect_clicked(move |button| {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let mut settings = draft.borrow().clone();
            settings.images = images.is_active();
            settings.audio_video = media.is_active();
            settings.excluded_extensions = extensions
                .text()
                .split(',')
                .map(|s| s.trim().trim_start_matches('.').to_lowercase())
                .filter(|s| !s.is_empty())
                .collect();
            settings.backend = match backend.selected() {
                1 => Backend::Cpu,
                2 => Backend::Vulkan,
                3 => Backend::Cuda,
                4 => Backend::Rocm,
                _ => Backend::Auto,
            };
            settings.rocm_override = match rocm.selected() {
                1 => Some("10.3.0"),
                2 => Some("11.0.0"),
                3 => Some("11.0.1"),
                4 => Some("12.0.0"),
                _ => None,
            }
            .map(str::to_owned);
            settings.external = if use_external.is_active() {
                Some(External {
                    url: address.text().to_string(),
                    model: model.text().to_string(),
                    allow_remote: consent.is_active(),
                })
            } else {
                None
            };
            settings.search_enabled = replace.is_active();
            if let Some(dialog) = close.upgrade() {
                ui.apply_settings(settings, dialog.upcast_ref(), button);
            }
        });
        dialog.present(Some(&self.window));
    }
}

fn clear_list(list: &gtk::ListBox) {
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
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
                "Exclude Folder"
            } else {
                "Choose Search Folder"
            })
            .modal(true)
            .build();
        let list = list.clone();
        let draft = draft.clone();
        let weak = weak.clone();
        picker.select_folder(Some(&parent), gio::Cancellable::NONE, move |result| {
            if let Ok(file) = result
                && let Some(path) = file.path()
            {
                let mut s = draft.borrow_mut();
                let folders = if excluded {
                    &mut s.excluded_folders
                } else {
                    &mut s.folders
                };
                if !folders.contains(&path) {
                    folders.push(path);
                }
                drop(s);
                if let Some(ui) = weak.upgrade() {
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
        let ui = Window::build(&app, Paths::from_env());
        ui.window.present();
        ui.refresh();
        until(|| {
            ui.window.is_mapped()
                && ui.main_stack.visible_child_name().as_deref() == Some("library")
        });
        ui.entry.set_text("the clause about pets in my lease");
        until(|| {
            ui.results.first_child().is_some()
                && ui.result_stack.visible_child_name().as_deref() == Some("results")
        });
        assert!(
            ui.results
                .first_child()
                .unwrap()
                .tooltip_text()
                .unwrap()
                .contains("Rental")
        );
        let output = std::env::var_os("GSEEK_SCREENSHOT_DIR").map(PathBuf::from);
        if let Some(dir) = &output {
            std::fs::create_dir_all(dir).unwrap();
            capture(&ui.window, &dir.join("search.png"));
        }
        ui.view_stack.set_visible_child_name("activity");
        ui.refresh();
        until(|| ui.logs.first_child().is_some());
        // Allow a full frame after changing pages before recording the native widget scene.
        let frame = Instant::now();
        until(|| frame.elapsed() > Duration::from_millis(300));
        if let Some(dir) = &output {
            capture(&ui.window, &dir.join("activity.png"));
        }
        ui.preferences(None);
        until(|| ui.window.visible_dialog().is_some());
        let frame = Instant::now();
        until(|| frame.elapsed() > Duration::from_millis(400));
        if let Some(dir) = &output {
            capture(&ui.window, &dir.join("preferences.png"));
        }
        close_dialog(&ui.window);
        ui.setup();
        until(|| ui.window.visible_dialog().is_some());
        let frame = Instant::now();
        until(|| frame.elapsed() > Duration::from_millis(400));
        if let Some(dir) = &output {
            capture(&ui.window, &dir.join("setup.png"));
        }
        close_dialog(&ui.window);
        ui.window.set_default_size(390, 700);
        ui.view_stack.set_visible_child_name("search");
        until(|| ui.window.width() <= 400);
        let frame = Instant::now();
        until(|| frame.elapsed() > Duration::from_millis(300));
        if let Some(dir) = &output {
            capture(&ui.window, &dir.join("search-narrow.png"));
        }
        ui.window.close();
    }
}
