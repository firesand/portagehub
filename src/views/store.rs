//! The "Store": discover packages — home, categories and search.

use super::common::{self, cover, package_row, page, subnav};
use crate::app::{Action, App, Tab};
use crate::data::Reload;
use crate::portage::emerge::{self, EmergeOptions};
use crate::ui::theme::{self, *};
use crate::ui::widgets as w;
use egui::{Color32, CornerRadius, FontId, Margin, RichText, Sense, Ui};
use egui_phosphor::regular as icon;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Page {
    #[default]
    Home,
    Categories,
    Search,
}

#[derive(Default)]
pub struct State {
    pub page: Page,
    pub query: String,
    pub show_detail: bool,
    pub category: Option<String>,
    pub in_desc: bool,
    pub only_installed: bool,
    pub repo_filter: Option<String>,
    cache_key: String,
    results: Vec<usize>,
}

pub fn show(app: &mut App, ui: &mut Ui) {
    if app.store.show_detail {
        super::details::show(app, ui, true);
        return;
    }
    let mut page_sel = app.store.page;
    let mut query = app.store.query.clone();
    let mut typed = false;
    subnav(ui, &mut page_sel, &[(Page::Home, "Home"), (Page::Categories, "Categories"), (Page::Search, "Search")], |ui| {
        let r = w::search_box(ui, &mut query, "Search packages (name or description)…", 340.0);
        typed = r.changed() && !query.is_empty();
    });
    if typed {
        page_sel = Page::Search;
    }
    app.store.page = page_sel;
    app.store.query = query;

    if app.data.catalog.is_none() {
        w::loading(ui, "Reading the package tree…");
        return;
    }
    match app.store.page {
        Page::Home => home(app, ui),
        Page::Categories => categories(app, ui),
        Page::Search => search(app, ui),
    }
}

fn hero(app: &mut App, ui: &mut Ui) {
    let width = ui.available_width();
    let height = 190.0;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, height), Sense::hover());
    let p = ui.painter();
    w::paint_hgradient(p, rect, Color32::from_rgb(0x1e, 0x4f, 0x78), Color32::from_rgb(0x2b, 0x2a, 0x5a));
    w::paint_vgradient(p, rect, Color32::from_black_alpha(0), Color32::from_black_alpha(90));
    p.text(rect.right_top() + egui::vec2(-60.0, 95.0), egui::Align2::CENTER_CENTER, icon::CUBE, FontId::proportional(150.0), Color32::from_white_alpha(18));
    let inner = rect.shrink2(egui::vec2(28.0, 22.0));
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(inner).layout(egui::Layout::top_down(egui::Align::Min)));
    let ui = &mut child;
    ui.label(RichText::new("WELCOME BACK").size(12.0).family(theme::bold()).color(ACCENT));
    ui.label(RichText::new("Your Gentoo system").size(30.0).family(theme::bold()).color(WHITE));
    let mut facts = Vec::new();
    if let Some(v) = &app.data.vdb {
        facts.push(format!("{} packages installed", v.pkgs.len()));
        facts.push(format!("{} in @world", v.world_raw.len()));
    }
    if let Some(u) = &app.data.updates {
        facts.push(format!("{} possible updates", u.len()));
    }
    if let Some(ts) = app.data.last_sync() {
        facts.push(format!("last sync {}", crate::fmt::ago(ts)));
    }
    ui.label(RichText::new(facts.join("  ·  ")).color(TEXT));
    ui.add_space(14.0);
    ui.horizontal(|ui| {
        if w::big_button(ui, &format!("{}  UPDATE SYSTEM", icon::ROCKET_LAUNCH), GRAD_GREEN, 200.0)
            .on_hover_text("Preview emerge -uDN @world")
            .clicked()
        {
            app.push(Action::Pretend((emerge::recipes()[0].build)()));
        }
        ui.add_space(6.0);
        if w::big_button(ui, &format!("{}  SYNC REPOSITORIES", icon::ARROWS_CLOCKWISE), GRAD_BLUE, 200.0)
            .on_hover_text("emerge --sync: download the latest ebuilds for all auto-sync repositories")
            .clicked()
        {
            app.push(Action::Emerge(EmergeOptions { action: emerge::Action::Sync, ..Default::default() }, vec![]));
        }
    });
}

fn home(app: &mut App, ui: &mut Ui) {
    page(ui, "store-home", |ui| {
        hero(app, ui);
        ui.add_space(12.0);
        common::tip(
            app,
            ui,
            "New to Gentoo? Packages are built from source by Portage. Browse here, open a package to see its versions and USE flags (optional features), then press INSTALL — PortageHub always shows you a preview of what emerge will do before anything changes.",
        );

        // Updates row
        let updates = app.data.updates.clone();
        w::section(ui, &format!("Updates available{}", updates.as_ref().map(|u| format!(" ({})", u.len())).unwrap_or_default()));
        match &updates {
            None => {
                ui.horizontal(|ui| {
                    ui.add(egui::Spinner::new().color(ACCENT));
                    ui.label(w::dim("Comparing installed packages with the tree…"));
                });
            }
            Some(u) if u.is_empty() => {
                ui.label(w::dim(format!("{} Everything installed is at the newest visible version (local estimate).", icon::CHECK_CIRCLE)));
            }
            Some(u) => {
                egui::ScrollArea::horizontal().id_salt("upd-row").show(ui, |ui| {
                    ui.horizontal_top(|ui| {
                        for c in u.iter().take(40) {
                            update_card(app, ui, &c.key, &c.installed, &c.available, &c.repo);
                        }
                    });
                });
                ui.label(w::dim("Estimate from keywords and your package.* files. The authoritative list comes from the Update preview."));
            }
        }

        // News
        if let Some(news) = app.data.news.clone() {
            let unread: Vec<_> = news.iter().filter(|n| n.unread).collect();
            if !unread.is_empty() {
                w::section(ui, &format!("Unread news ({})", unread.len()));
                for n in unread.iter().take(4) {
                    w::card_tinted(ORANGE).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(icon::NEWSPAPER).size(20.0).color(ORANGE));
                            ui.vertical(|ui| {
                                ui.label(w::strong(&n.title));
                                ui.label(w::dim(format!("{} · {}", n.posted, n.name)));
                            });
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if w::button(ui, icon::BOOK_OPEN, "Read").clicked() {
                                    app.config_view.open_news(&n.name);
                                    app.push(Action::Tab(Tab::Config));
                                }
                            });
                        });
                    });
                    ui.add_space(4.0);
                }
            }
        }

        // Category tiles
        w::section(ui, "Browse by category");
        let cats: Vec<(String, usize)> = app
            .data
            .catalog
            .as_ref()
            .map(|c| {
                let mut v: Vec<(String, usize)> = c.categories.iter().map(|(k, v)| (k.clone(), v.len())).collect();
                v.sort_by(|a, b| b.1.cmp(&a.1));
                v.into_iter().take(18).collect()
            })
            .unwrap_or_default();
        category_grid(app, ui, &cats);

        // Recently merged
        if let Some(h) = app.data.history.clone() {
            w::section(ui, "Recently installed or updated");
            let recent: Vec<_> = h.merges.iter().rev().filter(|m| m.ok).take(8).collect();
            for m in recent {
                let cat = m.key.split('/').next().unwrap_or("");
                let pn = m.key.split('/').nth(1).unwrap_or("");
                let r = package_row(
                    ui,
                    cat,
                    pn,
                    &format!("{} · built in {}", crate::fmt::ago(m.end), crate::portage::history::fmt_duration(m.duration())),
                    &m.ver,
                    if m.binary { "binary" } else { "source" },
                    None,
                    false,
                );
                if r.clicked() {
                    app.push(Action::Open(m.key.clone()));
                }
            }
        }
    });
}

fn update_card(app: &mut App, ui: &mut Ui, key: &str, from: &str, to: &str, repo: &str) {
    let (cat, pn) = key.split_once('/').unwrap_or(("", key));
    let size = egui::vec2(210.0, 150.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let p = ui.painter();
    let base = common::category_color(cat);
    let top = egui::Rect::from_min_size(rect.min, egui::vec2(size.x, 88.0));
    w::paint_vgradient(p, top, w::lerp_color(base, WHITE, 0.15), w::lerp_color(base, Color32::BLACK, 0.3));
    p.text(top.center(), egui::Align2::CENTER_CENTER, common::category_icon(cat), FontId::proportional(40.0), Color32::from_white_alpha(200));
    let bottom = egui::Rect::from_min_max(egui::pos2(rect.left(), top.bottom()), rect.max);
    p.rect_filled(bottom, CornerRadius::ZERO, if resp.hovered() { CARD_HOVER } else { CARD });
    let mut j = egui::text::LayoutJob::simple_singleline(pn.to_string(), FontId::new(14.0, theme::medium()), WHITE);
    j.wrap = egui::text::TextWrapping::truncate_at_width(size.x - 16.0);
    p.galley(bottom.min + egui::vec2(8.0, 6.0), ui.painter().layout_job(j), WHITE);
    p.text(bottom.min + egui::vec2(8.0, 30.0), egui::Align2::LEFT_TOP, format!("{from}  {}  {to}", icon::ARROW_RIGHT), FontId::proportional(12.0), GREEN_TEXT);
    p.text(bottom.min + egui::vec2(8.0, 46.0), egui::Align2::LEFT_TOP, format!("::{repo}"), FontId::proportional(11.0), TEXT_FAINT);
    if resp.hovered() {
        p.rect_stroke(rect, CornerRadius::ZERO, egui::Stroke::new(1.0, ACCENT), egui::StrokeKind::Inside);
    }
    if resp.clicked() {
        app.push(Action::Open(key.to_string()));
    }
    resp.context_menu(|ui| common::package_menu(app, ui, key, true));
}

fn category_grid(app: &mut App, ui: &mut Ui, cats: &[(String, usize)]) {
    let tile = egui::vec2(200.0, 74.0);
    let per_row = ((ui.available_width() + 8.0) / (tile.x + 8.0)).floor().max(1.0) as usize;
    for row in cats.chunks(per_row) {
        ui.horizontal_top(|ui| {
            for (c, n) in row {
                let (rect, resp) = ui.allocate_exact_size(tile, Sense::click());
                let base = common::category_color(c);
                let p = ui.painter();
                let (a, b) = if resp.hovered() {
                    (w::lerp_color(base, WHITE, 0.25), base)
                } else {
                    (w::lerp_color(base, WHITE, 0.08), w::lerp_color(base, Color32::BLACK, 0.4))
                };
                w::paint_hgradient(p, rect, a, b);
                p.text(rect.left_center() + egui::vec2(14.0, 0.0), egui::Align2::LEFT_CENTER, common::category_icon(c), FontId::proportional(26.0), Color32::from_white_alpha(230));
                p.text(rect.left_top() + egui::vec2(52.0, 18.0), egui::Align2::LEFT_TOP, c, FontId::new(14.0, theme::medium()), WHITE);
                p.text(rect.left_top() + egui::vec2(52.0, 40.0), egui::Align2::LEFT_TOP, format!("{n} packages"), FontId::proportional(12.0), Color32::from_white_alpha(170));
                if resp.clicked() {
                    app.store.category = Some(c.clone());
                    app.store.page = Page::Categories;
                }
            }
        });
        ui.add_space(4.0);
    }
}

fn categories(app: &mut App, ui: &mut Ui) {
    let Some(cat) = app.data.catalog.clone() else { return };
    if let Some(c) = app.store.category.clone() {
        let idx = cat.categories.get(&c).cloned().unwrap_or_default();
        let vdb = app.data.vdb.clone();
        egui::Frame::new().inner_margin(Margin::symmetric(24, 10)).show(ui, |ui| {
            ui.horizontal(|ui| {
                if w::button(ui, icon::ARROW_LEFT, "All categories").clicked() {
                    app.store.category = None;
                }
                cover(ui, &c, 34.0);
                ui.label(RichText::new(&c).size(22.0).family(theme::bold()).color(WHITE));
                ui.label(w::dim(format!("{} packages", idx.len())));
            });
            if let Some(repo_loc) = cat.repo_location("gentoo")
                && let Some(d) = crate::portage::catalog::category_description(repo_loc, &c)
            {
                ui.label(w::dim(d));
            }
        });
        let row_h = 54.0;
        egui::ScrollArea::vertical().id_salt("cat-list").auto_shrink([false, false]).show_rows(ui, row_h, idx.len(), |ui, range| {
            egui::Frame::new().inner_margin(Margin::symmetric(24, 0)).show(ui, |ui| {
                for i in range {
                    let p = &cat.pkgs[idx[i]];
                    let key = p.key();
                    let inst = vdb.as_ref().map(|v| v.installed(&key)).unwrap_or_default();
                    let inst_v = inst.last().map(|i| i.ver.raw.clone());
                    let latest = p.latest().map(|e| e.ver.raw.clone()).unwrap_or_default();
                    let repos: Vec<String> = p.repos().iter().map(|r| r.to_string()).collect();
                    let r = package_row(ui, &p.cat, &p.pn, &p.desc, &latest, &repos.join(" "), inst_v.as_deref(), false);
                    if r.clicked() {
                        app.push(Action::Open(key.clone()));
                    }
                    r.context_menu(|ui| common::package_menu(app, ui, &key, inst_v.is_some()));
                }
            });
        });
        return;
    }
    let mut cats: Vec<(String, usize)> = cat.categories.iter().map(|(k, v)| (k.clone(), v.len())).collect();
    let q = app.store.query.to_lowercase();
    if !q.is_empty() {
        cats.retain(|(c, _)| c.contains(&q));
    }
    page(ui, "cats", |ui| {
        w::title(ui, "Categories");
        w::subtitle(ui, &format!("{} categories across all repositories", cats.len()));
        ui.add_space(8.0);
        // Group by prefix (app-, dev-, media- …)
        let mut groups: std::collections::BTreeMap<String, Vec<(String, usize)>> = Default::default();
        for (c, n) in cats {
            let g = c.split('-').next().unwrap_or(&c).to_string();
            groups.entry(g).or_default().push((c, n));
        }
        for (g, list) in groups {
            w::section(ui, &g);
            category_grid(app, ui, &list);
        }
    });
}

fn run_search(app: &mut App) {
    let key = format!("{}|{}|{}|{:?}", app.store.query, app.store.in_desc, app.store.only_installed, app.store.repo_filter);
    if key == app.store.cache_key {
        return;
    }
    app.store.cache_key = key;
    let Some(cat) = app.data.catalog.clone() else { return };
    let vdb = app.data.vdb.clone();
    let q = app.store.query.trim().to_lowercase();
    let mut scored: Vec<(u32, usize)> = Vec::new();
    if q.is_empty() {
        app.store.results.clear();
        return;
    }
    let (qcat, qname) = match q.split_once('/') {
        Some((c, n)) => (Some(c.to_string()), n.to_string()),
        None => (None, q.clone()),
    };
    for (i, p) in cat.pkgs.iter().enumerate() {
        if let Some(c) = &qcat
            && !p.cat.contains(c.as_str())
        {
            continue;
        }
        if let Some(r) = &app.store.repo_filter
            && !p.ebuilds.iter().any(|e| &*e.repo == r)
        {
            continue;
        }
        let name = p.pn.to_lowercase();
        let score = if name == qname {
            1000
        } else if name.starts_with(&qname) {
            700 - (name.len() as u32).min(200)
        } else if name.contains(&qname) {
            400 - (name.len() as u32).min(200)
        } else if app.store.in_desc && p.desc.to_lowercase().contains(&qname) {
            100
        } else {
            continue;
        };
        if app.store.only_installed && !vdb.as_ref().is_some_and(|v| v.is_installed(&p.key())) {
            continue;
        }
        let bonus = if vdb.as_ref().is_some_and(|v| v.is_installed(&p.key())) { 50 } else { 0 };
        scored.push((score + bonus, i));
    }
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    app.store.results = scored.into_iter().map(|x| x.1).take(2000).collect();
}

fn search(app: &mut App, ui: &mut Ui) {
    run_search(app);
    let Some(cat) = app.data.catalog.clone() else { return };
    let vdb = app.data.vdb.clone();
    egui::Frame::new().inner_margin(Margin::symmetric(24, 10)).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("{} results", app.store.results.len())).size(18.0).family(theme::bold()).color(WHITE));
            if !app.store.query.is_empty() {
                ui.label(w::dim(format!("for “{}”", app.store.query)));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let mut repos: Vec<String> = cat.repo_locations.keys().cloned().collect();
                repos.sort();
                egui::ComboBox::from_id_salt("repo-filter")
                    .selected_text(app.store.repo_filter.clone().map(|r| format!("::{r}")).unwrap_or_else(|| "All repositories".into()))
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut app.store.repo_filter, None, "All repositories");
                        for r in repos {
                            ui.selectable_value(&mut app.store.repo_filter, Some(r.clone()), format!("::{r}"));
                        }
                    });
                ui.label(w::dim("Installed only"));
                w::toggle(ui, &mut app.store.only_installed);
                ui.label(w::dim("Search descriptions"));
                w::toggle(ui, &mut app.store.in_desc);
            });
        });
    });
    if app.store.query.trim().is_empty() {
        w::empty_state(ui, icon::MAGNIFYING_GLASS, "Search the tree", "Type a package name like “firefox”, or “www-client/” to list a category.");
        return;
    }
    if app.store.results.is_empty() {
        w::empty_state(ui, icon::SMILEY_SAD, "Nothing found", "Try enabling “Search descriptions”, or add an overlay that provides it.");
        if w::button(ui, icon::GIT_BRANCH, "Browse overlays").clicked() {
            app.push(Action::Tab(Tab::Overlays));
            app.push(Action::Reload(Reload::RemoteRepos));
        }
        return;
    }
    let results = app.store.results.clone();
    egui::ScrollArea::vertical().id_salt("search-res").auto_shrink([false, false]).show_rows(ui, 54.0, results.len(), |ui, range| {
        egui::Frame::new().inner_margin(Margin::symmetric(24, 0)).show(ui, |ui| {
            for i in range {
                let p = &cat.pkgs[results[i]];
                let key = p.key();
                let inst_v = vdb.as_ref().and_then(|v| v.installed(&key).last().map(|i| i.ver.raw.clone()));
                let latest = p.latest().map(|e| e.ver.raw.clone()).unwrap_or_default();
                let repos: Vec<String> = p.repos().iter().map(|r| format!("::{r}")).collect();
                let r = package_row(ui, &p.cat, &p.pn, &p.desc, &latest, &repos.join(" "), inst_v.as_deref(), false);
                if r.clicked() {
                    app.push(Action::Open(key.clone()));
                }
                r.context_menu(|ui| common::package_menu(app, ui, &key, inst_v.is_some()));
            }
        });
    });
}
