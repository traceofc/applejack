// Playlist editing: new playlist, add songs / albums to a playlist, and the edit dialog
// (rename, reorder by dragging or with the arrows, remove tracks). Changes go to Apple
// through the engine's createPlaylist / addToPlaylist / renamePlaylist / setPlaylistTracks.
use std::{cell::RefCell, rc::Rc};

use adw::prelude::*;
use gtk::{gdk, glib};
use serde_json::{Value, json};

use super::{App, is_favourites, widgets::label};
use crate::model::{Item, Kind};

/// {id, type} the playlist calls take for a song.
pub fn track_ref(it: &Item) -> Value {
    if it.is_library() || it.id.starts_with("i.") {
        json!({ "id": it.id, "type": "library-songs" })
    } else {
        json!({ "id": it.song_play_id(), "type": "songs" })
    }
}

fn text_entry(title: &str, text: &str) -> adw::EntryRow {
    adw::EntryRow::builder().title(title).text(text).build()
}

impl App {
    /// Playlists the user can add to: their own (Apple marks them canEdit).
    fn editable_playlists(&self) -> Vec<Item> {
        let all = self.st.borrow().playlists.clone();
        let known = all.iter().any(|p| p.can_edit);
        all.into_iter().filter(|p| !is_favourites(p) && p.id.starts_with("p.") && (p.can_edit || !known)).collect()
    }

    /// "New Playlist…": asks for a name, creates it (with `tracks`, if any) and opens it.
    pub fn new_playlist(self: &Rc<Self>, tracks: Vec<Value>) {
        let dialog = adw::AlertDialog::new(Some("New Playlist"), None);
        let name = text_entry("Name", "");
        let desc = text_entry("Description", "");
        let list = gtk::ListBox::new();
        list.add_css_class("boxed-list");
        list.set_selection_mode(gtk::SelectionMode::None);
        list.append(&name);
        list.append(&desc);
        dialog.set_extra_child(Some(&list));
        dialog.add_responses(&[("cancel", "Cancel"), ("create", "Create")]);
        dialog.set_response_appearance("create", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("create"));
        dialog.set_close_response("cancel");
        dialog.set_response_enabled("create", false);
        let d = dialog.downgrade();
        name.connect_changed(move |e| {
            if let Some(d) = d.upgrade() {
                d.set_response_enabled("create", !e.text().trim().is_empty());
            }
        });
        name.set_activates_default(true);
        let app = self.clone();
        let (n, ds) = (name.clone(), desc.clone());
        dialog.connect_response(None, move |_, r| {
            if r != "create" {
                return;
            }
            let title = n.text().trim().to_owned();
            if title.is_empty() {
                return;
            }
            let fut = app.engine.call(
                "createPlaylist",
                json!({ "name": title, "description": ds.text().trim(), "tracks": tracks }),
            );
            let app = app.clone();
            glib::spawn_future_local(async move {
                match fut.await {
                    Ok(v) => {
                        app.toast(&format!("Created “{title}”"));
                        if let Ok(mut it) = serde_json::from_value::<Item>(v) {
                            if !it.id.is_empty() {
                                it.can_edit = true;
                                it.title.get_or_insert(title.clone());
                                app.show_added(&it, "playlists");
                                let pls = app.cache.get::<Vec<Item>>("library:playlists").unwrap_or_default();
                                app.set_sidebar_playlists(&pls);
                                app.select_sidebar(&format!("pl:{}", it.id));
                            }
                        }
                        for secs in [2, 8] {
                            let app = app.clone();
                            glib::timeout_add_seconds_local_once(secs, move || app.sync_library());
                        }
                    }
                    Err(e) => app.error(&format!("couldn’t create the playlist: {e}")),
                }
            });
        });
        dialog.present(Some(&self.window));
        name.grab_focus();
    }

    /// "Add to Playlist…" for a song or an album.
    pub fn add_to_playlist(self: &Rc<Self>, it: &Item) {
        let dialog = adw::Dialog::builder().title("Add to Playlist").content_width(380).content_height(520).build();
        let list = gtk::ListBox::new();
        list.add_css_class("boxed-list");
        list.set_selection_mode(gtk::SelectionMode::None);
        list.set_margin_top(6);
        list.set_margin_bottom(18);
        list.set_margin_start(18);
        list.set_margin_end(18);
        list.set_valign(gtk::Align::Start);
        let new_row = adw::ActionRow::builder().title("New Playlist…").activatable(true).build();
        new_row.add_prefix(&gtk::Image::from_icon_name("list-add-symbolic"));
        list.append(&new_row);
        let playlists = self.editable_playlists();
        for p in &playlists {
            let row = adw::ActionRow::builder().title(glib::markup_escape_text(p.title()).as_str()).activatable(true).build();
            row.add_prefix(&gtk::Image::from_icon_name("view-list-symbolic"));
            row.set_widget_name(&p.id);
            list.append(&row);
        }
        let tv = adw::ToolbarView::new();
        tv.add_top_bar(&adw::HeaderBar::new());
        tv.set_content(Some(&gtk::ScrolledWindow::builder().child(&list).hscrollbar_policy(gtk::PolicyType::Never).vexpand(true).build()));
        dialog.set_child(Some(&tv));

        let (app, it, d) = (self.clone(), it.clone(), dialog.downgrade());
        list.connect_row_activated(move |_, row| {
            if let Some(d) = d.upgrade() {
                d.close();
            }
            let target = if row.widget_name().starts_with("p.") {
                playlists.iter().find(|p| p.id == row.widget_name()).cloned()
            } else {
                None
            };
            let app = app.clone();
            let it = it.clone();
            glib::spawn_future_local(async move {
                let tracks = match app.track_refs_of(&it).await {
                    Ok(t) => t,
                    Err(e) => return app.error(&e),
                };
                match target {
                    None => app.new_playlist(tracks),
                    Some(p) => app.append_to_playlist(&p, tracks),
                }
            });
        });
        dialog.present(Some(&self.window));
    }

    /// The songs of an item, as playlist track refs (an album is fetched).
    async fn track_refs_of(self: &Rc<Self>, it: &Item) -> Result<Vec<Value>, String> {
        match it.kind() {
            Kind::Album | Kind::Playlist => {
                let t = if it.kind() == Kind::Album { "album" } else { "playlist" };
                let v = self.engine.call("tracks", json!({ "id": it.id, "type": t })).await?;
                let items: Vec<Item> = serde_json::from_value(v).map_err(|e| e.to_string())?;
                Ok(items.iter().map(track_ref).collect())
            }
            _ => Ok(vec![track_ref(it)]),
        }
    }

    fn append_to_playlist(self: &Rc<Self>, p: &Item, tracks: Vec<Value>) {
        let app = self.clone();
        let (id, title) = (p.id.clone(), p.title().to_owned());
        let fut = self.engine.call("addToPlaylist", json!({ "id": id, "tracks": tracks }));
        glib::spawn_future_local(async move {
            match fut.await {
                Ok(_) => {
                    app.toast(&format!("Added to “{title}”"));
                    app.cache.put(&format!("container:playlist:{id}"), &Value::Null);
                    // On screen: show the new tracks (Apple lists them after a moment).
                    if app.sidebar.selected_row().is_some_and(|r| r.widget_name() == format!("pl:{id}")) {
                        let app = app.clone();
                        glib::timeout_add_seconds_local_once(2, move || app.show_section(&format!("pl:{id}")));
                    }
                }
                Err(e) => app.error(&format!("couldn’t add to the playlist: {e}")),
            }
        });
    }

    /// The edit dialog. `current` = name, description and tracks as shown; `saved` gets the
    /// new ones after Apple took them.
    pub fn edit_playlist(
        self: &Rc<Self>,
        id: &str,
        name: &str,
        description: &str,
        tracks: Vec<Item>,
        saved: impl Fn(&str, &str, &[Item]) + 'static,
    ) {
        let dialog = adw::Dialog::builder().title("Edit Playlist").content_width(560).content_height(720).build();
        let cancel = gtk::Button::with_label("Cancel");
        let save = gtk::Button::with_label("Save");
        save.add_css_class("suggested-action");
        let header = adw::HeaderBar::new();
        header.set_show_start_title_buttons(false);
        header.set_show_end_title_buttons(false);
        header.pack_start(&cancel);
        header.pack_end(&save);

        let name_row = text_entry("Name", name);
        let desc_row = text_entry("Description", description);
        let fields = gtk::ListBox::new();
        fields.add_css_class("boxed-list");
        fields.set_selection_mode(gtk::SelectionMode::None);
        fields.append(&name_row);
        fields.append(&desc_row);

        let count = label("", &["heading"]);
        count.set_margin_top(18);
        let hint = label("Drag tracks to reorder them", &["dim-label", "caption"]);
        let list = gtk::ListBox::new();
        list.add_css_class("boxed-list");
        list.set_selection_mode(gtk::SelectionMode::None);
        list.set_margin_top(6);

        let col = gtk::Box::new(gtk::Orientation::Vertical, 4);
        col.set_margin_top(6);
        col.set_margin_bottom(24);
        col.set_margin_start(18);
        col.set_margin_end(18);
        col.append(&fields);
        col.append(&count);
        col.append(&hint);
        col.append(&list);
        let clamp = adw::Clamp::builder().maximum_size(640).child(&col).build();
        let tv = adw::ToolbarView::new();
        tv.add_top_bar(&header);
        tv.set_content(Some(&gtk::ScrolledWindow::builder().child(&clamp).hscrollbar_policy(gtk::PolicyType::Never).vexpand(true).build()));
        dialog.set_child(Some(&tv));
        dialog.set_can_close(true);

        let items = Rc::new(RefCell::new(tracks.clone()));
        let show_count = {
            let (count, items) = (count.clone(), items.clone());
            move || count.set_label(&crate::model::count(items.borrow().len(), "song"))
        };
        show_count();
        let show_count = Rc::new(show_count);
        for it in tracks.iter() {
            list.append(&edit_row(it, &list, &items, show_count.clone()));
        }

        let d = dialog.downgrade();
        cancel.connect_clicked(move |_| {
            if let Some(d) = d.upgrade() {
                d.close();
            }
        });
        let (app, id, orig_name, orig_desc, orig_ids, d) = (
            self.clone(),
            id.to_owned(),
            name.to_owned(),
            description.to_owned(),
            tracks.iter().map(|t| t.id.clone()).collect::<Vec<_>>(),
            dialog.downgrade(),
        );
        let saved = Rc::new(saved);
        save.connect_clicked(move |b| {
            let new_name = name_row.text().trim().to_owned();
            if new_name.is_empty() {
                app.toast("A playlist needs a name");
                return;
            }
            let new_desc = desc_row.text().trim().to_owned();
            let now: Vec<Item> = items.borrow().clone();
            let renamed = new_name != orig_name || new_desc != orig_desc.trim();
            let reordered = now.iter().map(|t| t.id.as_str()).ne(orig_ids.iter().map(String::as_str));
            if let Some(d) = d.upgrade() {
                d.close();
            }
            if !renamed && !reordered {
                return;
            }
            b.set_sensitive(false);
            let rename = renamed.then(|| app.engine.call("renamePlaylist", json!({ "id": id, "name": new_name, "description": new_desc })));
            let reorder = reordered.then(|| {
                app.engine.call("setPlaylistTracks", json!({ "id": id, "tracks": now.iter().map(track_ref).collect::<Vec<_>>() }))
            });
            let (app, id, saved) = (app.clone(), id.clone(), saved.clone());
            glib::spawn_future_local(async move {
                let mut errors = Vec::new();
                if let Some(f) = rename {
                    if let Err(e) = f.await {
                        errors.push(e);
                    }
                }
                if let Some(f) = reorder {
                    if let Err(e) = f.await {
                        errors.push(e);
                    }
                }
                app.cache.put(&format!("container:playlist:{id}"), &Value::Null);
                if errors.is_empty() {
                    app.toast("Playlist saved");
                    saved(&new_name, &new_desc, &now);
                } else {
                    app.error(&format!("couldn’t save the playlist: {}", errors.join("; ")));
                }
                if renamed {
                    for secs in [2, 8] {
                        let app = app.clone();
                        glib::timeout_add_seconds_local_once(secs, move || app.sync_library());
                    }
                }
            });
        });
        dialog.present(Some(&self.window));
    }
}

/// One track in the edit dialog: drag handle, title, up / down, remove.
fn edit_row(it: &Item, list: &gtk::ListBox, items: &Rc<RefCell<Vec<Item>>>, changed: Rc<impl Fn() + 'static>) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .title(glib::markup_escape_text(it.title()).as_str())
        .subtitle(glib::markup_escape_text(it.artist()).as_str())
        .title_lines(1)
        .subtitle_lines(1)
        .build();
    let handle = gtk::Image::from_icon_name("list-drag-handle-symbolic");
    handle.add_css_class("dim-label");
    row.add_prefix(&handle);
    let up = gtk::Button::from_icon_name("go-up-symbolic");
    let down = gtk::Button::from_icon_name("go-down-symbolic");
    let remove = gtk::Button::from_icon_name("list-remove-symbolic");
    up.set_tooltip_text(Some("Move Up"));
    down.set_tooltip_text(Some("Move Down"));
    remove.set_tooltip_text(Some("Remove from Playlist"));
    for b in [&up, &down, &remove] {
        b.add_css_class("flat");
        b.add_css_class("circular");
        b.set_valign(gtk::Align::Center);
        row.add_suffix(b);
    }

    let mover = {
        let (list, items) = (list.downgrade(), items.clone());
        move |row: &adw::ActionRow, to: i32| {
            let Some(list) = list.upgrade() else { return };
            let from = row.index();
            let n = items.borrow().len() as i32;
            let to = to.clamp(0, n - 1);
            if from < 0 || from == to {
                return;
            }
            let it = items.borrow_mut().remove(from as usize);
            items.borrow_mut().insert(to as usize, it);
            list.remove(row);
            list.insert(row, to);
        }
    };
    let mover = Rc::new(mover);
    let (m, r) = (mover.clone(), row.downgrade());
    up.connect_clicked(move |_| {
        if let Some(r) = r.upgrade() {
            m(&r, r.index() - 1);
        }
    });
    let (m, r) = (mover.clone(), row.downgrade());
    down.connect_clicked(move |_| {
        if let Some(r) = r.upgrade() {
            m(&r, r.index() + 1);
        }
    });
    let (list_w, items_c, r) = (list.downgrade(), items.clone(), row.downgrade());
    remove.connect_clicked(move |_| {
        let (Some(list), Some(r)) = (list_w.upgrade(), r.upgrade()) else { return };
        let i = r.index();
        if i >= 0 {
            items_c.borrow_mut().remove(i as usize);
            list.remove(&r);
            changed();
        }
    });

    // Drag a row onto another to move it there.
    let source = gtk::DragSource::new();
    source.set_actions(gdk::DragAction::MOVE);
    let r = row.downgrade();
    source.connect_prepare(move |_, _, _| {
        let r = r.upgrade()?;
        Some(gdk::ContentProvider::for_value(&r.index().to_string().to_value()))
    });
    let r = row.downgrade();
    source.connect_drag_begin(move |s, _| {
        if let Some(r) = r.upgrade() {
            s.set_icon(Some(&gtk::WidgetPaintable::new(Some(&r))), 20, 20);
        }
    });
    row.add_controller(source);
    let target = gtk::DropTarget::new(glib::Type::STRING, gdk::DragAction::MOVE);
    let (r, m, list_w) = (row.downgrade(), mover, list.downgrade());
    target.connect_drop(move |_, v, _, _| {
        let (Some(r), Some(list)) = (r.upgrade(), list_w.upgrade()) else { return false };
        let Some(from) = v.get::<String>().ok().and_then(|s| s.parse::<i32>().ok()) else { return false };
        let Some(src) = list.row_at_index(from).and_downcast::<adw::ActionRow>() else { return false };
        m(&src, r.index());
        true
    });
    row.add_controller(target);
    row
}
