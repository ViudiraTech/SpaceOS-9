// Drawing layer for Space 商店, included from store.rs so it can share the
// window state without exposing it. Zero skin checks live here.

fn draw_store(
    canvas: &mut Canvas,
    text: &mut TextRenderer,
    theme: &Theme,
    state: &mut StoreState,
    content: Rect,
    prev_hits: &Hits,
    hits: &mut Hits,
) -> Option<Rect> {
    canvas.clear(theme.window);
    let sidebar = Rect::new(content.x, content.y, SIDEBAR_W, content.h);
    let topbar = Rect::new(
        content.x + SIDEBAR_W,
        content.y,
        content.w - SIDEBAR_W,
        TOPBAR_H,
    );
    let main_h = content.h - TOPBAR_H - if state.queue_open { QUEUE_H } else { 0 };
    let main = Rect::new(
        content.x + SIDEBAR_W,
        content.y + TOPBAR_H,
        content.w - SIDEBAR_W,
        main_h.max(1),
    );

    let mut ime = draw_sidebar(canvas, text, theme, state, sidebar, prev_hits, hits);
    draw_topbar(canvas, text, theme, state, topbar, prev_hits, hits);

    if state.detail.is_some() {
        draw_detail(canvas, text, theme, state, main, prev_hits, hits);
    } else {
        match state.view {
            NavSection::Discover => {
                draw_discover(canvas, text, theme, state, main, prev_hits, hits);
            }
            NavSection::Browse => draw_browse(canvas, text, theme, state, main, prev_hits, hits),
            NavSection::Updates => {
                draw_updates(canvas, text, theme, state, main, prev_hits, hits);
            }
            NavSection::Installed => {
                draw_installed(canvas, text, theme, state, main, prev_hits, hits);
            }
            NavSection::Maintenance => {
                if let Some(spot) =
                    draw_maintenance(canvas, text, theme, state, main, prev_hits, hits)
                {
                    ime = Some(spot);
                }
            }
        }
    }

    if state.queue_open {
        let queue = Rect::new(content.x, content.y + content.h - QUEUE_H, content.w, QUEUE_H);
        draw_queue(canvas, text, theme, state, queue, prev_hits, hits);
    }

    if let Some((pkg_name, anchor)) = state.open_menu.clone() {
        draw_pkg_menu_overlay(
            canvas,
            text,
            theme,
            state,
            content,
            &pkg_name,
            anchor,
            prev_hits,
            hits,
        );
    }

    if let Some(preview) = state.preview.as_ref() {
        draw_preview(
            canvas,
            text,
            theme,
            content,
            preview,
            prev_hits,
            hits,
            state.last_pointer,
        );
    }
    if let Some(auth) = state.auth.as_ref() {
        ime = draw_auth(
            canvas,
            text,
            theme,
            content,
            auth,
            prev_hits,
            hits,
            state.last_pointer,
        );
    }
    if let Some((message, at)) = state.toast.clone() {
        draw_toast(canvas, text, theme, content, &message, at, hits);
    }
    ime
}

fn draw_sidebar(
    canvas: &mut Canvas,
    text: &mut TextRenderer,
    theme: &Theme,
    state: &StoreState,
    rect: Rect,
    prev_hits: &Hits,
    hits: &mut Hits,
) -> Option<Rect> {
    ui::sidebar_surface(canvas, theme, rect);
    let pointer = state.last_pointer;

    let brand_h = 64;
    ui_cards::icon_badge(
        canvas,
        Icon::Store,
        Rect::new(rect.x + 14, rect.y + 14, 34, 34),
        [theme.accent, ui_cards::mix(theme.accent, 0x1d4ed8, 0.35)],
    );
    ui_cards::draw_text(
        text,
        canvas,
        "Space 商店",
        rect.x + 56,
        rect.y + 36,
        15.5,
        theme.text,
        Face::Bold,
    );

    let search = Rect::new(rect.x + 12, rect.y + brand_h, rect.w - 24, 32);
    let search_focused = state.active_focus() == FocusedInput::Search;
    let search_preedit = if search_focused {
        state.preedit.as_str()
    } else {
        ""
    };
    let search_caret = ui::text_input_ime(
        canvas,
        text,
        theme,
        search,
        &state.search,
        search_preedit,
        "搜索软件包",
        search_focused,
        Some(Icon::Search),
    );
    hits.push(search, Action::FocusInput(FocusedInput::Search));
    if !state.search.is_empty() {
        let clr = Rect::new(search.right() - 24, search.y + 4, 20, 24);
        let hov = prev_hits.hover_rect(pointer.0, pointer.1, clr);
        if hov {
            canvas.rounded_rect(clr, 6, theme.panel_hover, 0.85);
        }
        let tw = text.measure("×", 13.0, Face::Bold);
        text.draw(
            canvas,
            "×",
            (clr.x + clr.w / 2) as f32 - tw / 2.0,
            (clr.y + 16) as f32,
            13.0,
            theme.text_muted,
            Face::Bold,
        );
        hits.push(clr, Action::ClearSearch);
    }

    let entries: [(NavSection, Icon, &str, usize); 5] = [
        (NavSection::Discover, Icon::Store, "探索精选", 0),
        (NavSection::Browse, Icon::Applications, "分类浏览", 0),
        (NavSection::Updates, Icon::Refresh, "软件更新", state.updates.len()),
        (NavSection::Installed, Icon::Folder, "已安装", state.installed_count()),
        (NavSection::Maintenance, Icon::Settings, "仓库与维护", 0),
    ];
    let mut y = rect.y + brand_h + 44;
    ui_cards::draw_text(
        text,
        canvas,
        "浏览与发现",
        rect.x + 18,
        y + 12,
        10.5,
        theme.text_muted,
        Face::Bold,
    );
    y += 20;
    for (idx, (view, icon, label, count)) in entries.into_iter().enumerate() {
        if idx == 2 {
            y += 10;
            ui_cards::draw_text(
                text,
                canvas,
                "管理与维护",
                rect.x + 18,
                y + 12,
                10.5,
                theme.text_muted,
                Face::Bold,
            );
            y += 20;
        }
        let item = Rect::new(rect.x + 10, y, rect.w - 20, 36);
        let active = state.view == view && state.detail.is_none();
        let hovered = prev_hits.hover_rect(pointer.0, pointer.1, item);
        ui_cards::nav_item(
            canvas,
            text,
            theme,
            item,
            icon,
            label,
            if count > 0 { Some(count) } else { None },
            active,
            hovered,
        );
        hits.push(item, Action::Nav(view));
        y += 40;
    }

    // Running jobs mini card at the bottom
    let running: Vec<&TxJob> = state.jobs.iter().filter(|j| j.status.is_active()).collect();
    let card = Rect::new(rect.x + 12, rect.y + rect.h - 78, rect.w - 24, 64);
    if !running.is_empty() {
        ui_cards::elevated_card(canvas, theme, card, false);
        let job = running[0];
        ui_cards::draw_text_clipped(
            text,
            canvas,
            &job.title,
            card.x + 10,
            card.y + 20,
            card.w - 20,
            12.0,
            theme.text,
            Face::Bold,
        );
        ui_cards::draw_text_clipped(
            text,
            canvas,
            &job.phase_msg,
            card.x + 10,
            card.y + 36,
            card.w - 20,
            11.0,
            theme.text_muted,
            Face::Ui,
        );
        ui::progress_bar(
            canvas,
            theme,
            Rect::new(card.x + 10, card.y + 46, card.w - 20, 6),
            job.percent as f32 / 100.0,
            Some(theme.accent),
        );
        hits.push(card, Action::ToggleQueue);
    }

    search_focused.then_some(search_caret)
}

fn draw_topbar(
    canvas: &mut Canvas,
    text: &mut TextRenderer,
    theme: &Theme,
    state: &StoreState,
    rect: Rect,
    prev_hits: &Hits,
    hits: &mut Hits,
) {
    canvas.fill(rect, theme.window_high);
    canvas.fill(
        Rect::new(rect.x, rect.bottom() - 1, rect.w, 1),
        ui_cards::soft_border(theme),
    );
    let pointer = state.last_pointer;
    let mut x = rect.x + 22;
    let btn_y = rect.y + (rect.h - 30) / 2;

    if state.detail.is_some() {
        let back = Rect::new(x, btn_y, 30, 30);
        let hovered = prev_hits.hover_rect(pointer.0, pointer.1, back);
        ui::icon_button(canvas, theme, back, Icon::GoBack, true, hovered);
        hits.push(back, Action::Back);
        x += 38;
    }

    let mut right = rect.right() - 20;
    let active_jobs = state.jobs.iter().filter(|j| j.status.is_active()).count();
    let queue_label = if state.queue_open {
        "收起任务".to_string()
    } else if active_jobs > 0 {
        format!("任务 · {active_jobs}")
    } else {
        "任务".to_string()
    };
    let queue_w = (text.measure(&queue_label, 12.0, Face::Bold) as i32 + 28).max(64);
    let queue = Rect::new(right - queue_w, btn_y, queue_w, 30);
    let hovered = prev_hits.hover_rect(pointer.0, pointer.1, queue);
    ui_cards::cta_pill(
        canvas,
        text,
        theme,
        queue,
        &queue_label,
        state.queue_open || active_jobs > 0,
        hovered,
        false,
        None,
    );
    hits.push(queue, Action::ToggleQueue);
    right = queue.x - 8;

    let refresh = Rect::new(right - 32, btn_y, 32, 30);
    let hovered = prev_hits.hover_rect(pointer.0, pointer.1, refresh);
    ui_cards::toolbar_icon_button(
        canvas,
        theme,
        refresh,
        Icon::Refresh,
        state.loading,
        hovered,
        !state.loading,
    );
    if !state.loading {
        hits.push(refresh, Action::RefreshCatalog);
    }
    right = refresh.x - 8;

    let status_chip: Option<(&str, u32, Option<Action>)> = if state.loading {
        Some(("同步中", theme.accent, None))
    } else if state.offline {
        Some(("离线", theme.warning, Some(Action::RefreshCatalog)))
    } else if !state.repos.is_empty() {
        Some(("在线", theme.success, Some(Action::Nav(NavSection::Maintenance))))
    } else {
        None
    };

    if let Some((label, color, action)) = status_chip {
        let chip_w = (text.measure(label, 11.5, Face::Bold) as i32 + 32).max(56);
        let min_title_w = 140;
        if right - chip_w - x >= min_title_w {
            let chip = Rect::new(right - chip_w, btn_y + 2, chip_w, 26);
            let hovered = action.is_some() && prev_hits.hover_rect(pointer.0, pointer.1, chip);
            ui_cards::status_dot_chip(canvas, text, theme, chip, label, color, hovered);
            if let Some(act) = action {
                hits.push(chip, act);
            }
            right = chip.x - 12;
        }
    }

    let title = match (state.detail.as_deref(), state.view) {
        (Some(name), _) => state
            .package(name)
            .map(|p| state.presentation(p).display_name)
            .unwrap_or_else(|| name.to_string()),
        (None, NavSection::Discover) => "探索精选".to_string(),
        (None, NavSection::Browse) => "分类浏览".to_string(),
        (None, NavSection::Updates) => "软件更新".to_string(),
        (None, NavSection::Installed) => "已安装".to_string(),
        (None, NavSection::Maintenance) => "仓库与维护".to_string(),
    };
    let avail_w = (right - x).max(60);
    ui_cards::draw_text_clipped(
        text,
        canvas,
        &title,
        x,
        rect.y + 34,
        avail_w,
        16.5,
        theme.text,
        Face::Bold,
    );
}

fn content_scroll_area(main: Rect) -> Rect {
    Rect::new(main.x + 22, main.y + 14, (main.w - 44).max(40), (main.h - 28).max(40))
}

fn draw_discover(
    canvas: &mut Canvas,
    text: &mut TextRenderer,
    theme: &Theme,
    state: &mut StoreState,
    main: Rect,
    prev_hits: &Hits,
    hits: &mut Hits,
) {
    let area = content_scroll_area(main);
    let inner_w = (area.w - SCROLLBAR_GUTTER).max(160);
    let offset = state.discover_scroll.offset_i32();
    let slides = state.hero_slides();
    let sections = &state.sections;
    let pointer = state.last_pointer;
    let total_h: i32;

    {
        let state_ref: &StoreState = state;
        let mut y = area.y - offset;
        canvas.with_clip(area, |canvas| {
            if !slides.is_empty() {
                let hero = Rect::new(area.x, y, inner_w, HERO_H);
                if hero.bottom() >= area.y && hero.y <= area.bottom() {
                    let index = state_ref.hero_index.min(slides.len() - 1);
                    let slide = &slides[index];
                    let installed = slide
                        .target_package
                        .as_deref()
                        .and_then(|name| state_ref.package(name))
                        .is_some_and(|pkg| pkg.installed);
                    let cta_approx = Rect::new(hero.x + 28, hero.bottom() - 56, 124, 32);
                    let prev_approx = Rect::new(cta_approx.right() + 14, cta_approx.y + 2, 28, 28);
                    let next_approx = Rect::new(prev_approx.right() + 8, cta_approx.y + 2, 28, 28);
                    let banner = state_ref.banner_sprite(slide.banner.as_deref());
                    let (cta, prev_r, next_r) = ui_cards::hero_card(
                        canvas,
                        text,
                        theme,
                        hero,
                        slide,
                        banner,
                        slides.len(),
                        installed,
                        prev_hits.hover_rect(pointer.0, pointer.1, cta_approx),
                        prev_hits.hover_rect(pointer.0, pointer.1, prev_approx),
                        prev_hits.hover_rect(pointer.0, pointer.1, next_approx),
                    );
                    hits.push_clipped(cta, area, Action::HeroCta(index));
                    if !prev_r.is_empty() {
                        hits.push_clipped(prev_r, area, Action::HeroPrev);
                        hits.push_clipped(next_r, area, Action::HeroNext);
                    }

                    let dots_y = hero.bottom() - 18;
                    for dot in 0..slides.len() {
                        let rect = Rect::new(hero.x + 28 + dot as i32 * 18, dots_y, 8, 8);
                        let color = if dot == index {
                            0xffffff
                        } else {
                            ui_cards::mix(0xffffff, slide.grad_right, 0.55)
                        };
                        canvas.rounded_rect(
                            rect,
                            4,
                            color,
                            if dot == index { 1.0 } else { 0.45 },
                        );
                        hits.push_clipped(
                            Rect::new(rect.x - 4, rect.y - 6, 16, 20),
                            area,
                            Action::HeroDot(dot),
                        );
                    }
                }
                y += HERO_H + 18;
            }

            if !state_ref.updates.is_empty() {
                let banner = Rect::new(area.x, y, inner_w, 54);
                if banner.bottom() >= area.y && banner.y <= area.bottom() {
                    ui_cards::elevated_card(canvas, theme, banner, false);
                    ui_cards::icon_badge(
                        canvas,
                        Icon::Refresh,
                        Rect::new(banner.x + 12, banner.y + 11, 32, 32),
                        [theme.accent, ui_cards::mix(theme.accent, 0x0284c7, 0.45)],
                    );
                    ui_cards::draw_text_clipped(
                        text,
                        canvas,
                        &format!("{} 个软件包有新版本可用", state_ref.updates.len()),
                        banner.x + 56,
                        banner.y + 32,
                        banner.w - 190,
                        13.5,
                        theme.text,
                        Face::Bold,
                    );
                    let cta = Rect::new(banner.right() - 118, banner.y + 13, 104, 28);
                    let hovered = prev_hits.hover_rect(pointer.0, pointer.1, cta);
                    ui_cards::cta_pill(
                        canvas, text, theme, cta, "查看更新", true, hovered, false, None,
                    );
                    hits.push_clipped(cta, area, Action::Nav(NavSection::Updates));
                }
                y += 66;
            }

            let columns = if inner_w >= 820 {
                3
            } else if inner_w >= 520 {
                2
            } else {
                1
            };
            let card_w = (inner_w - (columns - 1) * 16) / columns;

            for section in sections {
                let header = Rect::new(area.x, y, inner_w, 28);
                if header.bottom() >= area.y && header.y <= area.bottom() {
                    ui_cards::section_header(
                        canvas,
                        text,
                        theme,
                        header,
                        &section.title,
                        "",
                    );
                }
                y += 32;

                let mut visible_count = 0i32;
                for name in &section.packages {
                    let Some(pkg) = state_ref.package(name) else {
                        continue;
                    };
                    let col = visible_count % columns;
                    let row = visible_count / columns;
                    visible_count += 1;
                    let card = Rect::new(
                        area.x + col * (card_w + 16),
                        y + row * (FEATURED_CARD_H + 14),
                        card_w,
                        FEATURED_CARD_H,
                    );
                    if card.bottom() < area.y || card.y > area.bottom() {
                        continue;
                    }
                    let pres = state_ref.presentation(pkg);
                    let card_hov = prev_hits.hover_rect(pointer.0, pointer.1, card);
                    let cta_approx = Rect::new(card.right() - 88, card.bottom() - 36, 78, 26);
                    let cta_hov = prev_hits.hover_rect(pointer.0, pointer.1, cta_approx);
                    let progress = state_ref
                        .job_for(&pkg.name)
                        .map(|job| job.percent as f32 / 100.0);
                    let banner = state_ref.banner_sprite(pres.banner.as_deref());
                    let cta = ui_cards::featured_package_card(
                        canvas, text, theme, card, pkg, &pres, banner, card_hov, cta_hov, progress,
                    );
                    hits.push_clipped(card, area, Action::OpenDetail(pkg.name.clone()));
                    if state_ref.job_for(&pkg.name).is_some() {
                        hits.push_clipped(cta, area, Action::ToggleQueue);
                    } else if pkg.upgrade_available {
                        hits.push_clipped(cta, area, Action::Update(pkg.name.clone()));
                    } else if pkg.installed {
                        hits.push_clipped(cta, area, Action::OpenDetail(pkg.name.clone()));
                    } else {
                        hits.push_clipped(cta, area, Action::Install(pkg.name.clone()));
                    }
                }
                let rows = (visible_count + columns - 1) / columns;
                y += rows * (FEATURED_CARD_H + 14) + 12;
            }
        });
        total_h = (y + offset - area.y).max(area.h);
    }

    let sb_rect = Rect::new(area.right() - 8, area.y, 8, area.h);
    state
        .discover_scroll
        .set_bounds(area.h as f32, total_h as f32, sb_rect);
    ui::scrollbar_v_animated(
        canvas,
        theme,
        "store_discover_scrollbar",
        sb_rect,
        total_h as f32,
        area.h as f32,
        offset as f32,
        state.discover_scroll.is_hovered(),
        state.discover_scroll.is_dragging(),
        state.discover_scroll.is_animating(),
    );
}

fn draw_browse(
    canvas: &mut Canvas,
    text: &mut TextRenderer,
    theme: &Theme,
    state: &mut StoreState,
    main: Rect,
    prev_hits: &Hits,
    hits: &mut Hits,
) {
    let area = content_scroll_area(main);
    let pointer = state.last_pointer;
    let mut y = area.y;

    // Segmented Category Bar
    let mut cat_widths = [0i32; 4];
    let mut cat_labels: [String; 4] = Default::default();
    let mut cat_total_w = 6;
    for (idx, category) in StoreCategory::ALL.iter().enumerate() {
        let label = category_label(&state.catalog, *category);
        let w = text.measure(&label, 12.0, Face::Bold) as i32 + 26;
        cat_widths[idx] = w;
        cat_labels[idx] = label;
        cat_total_w += w + 2;
    }
    let cat_track = Rect::new(area.x, y, cat_total_w, 34);
    ui_cards::segmented_track(canvas, theme, cat_track);
    let mut cx = cat_track.x + 3;
    for (idx, category) in StoreCategory::ALL.iter().enumerate() {
        let w = cat_widths[idx];
        let pill = Rect::new(cx, cat_track.y + 3, w, 28);
        let selected = state.category == *category;
        let hovered = prev_hits.hover_rect(pointer.0, pointer.1, pill);
        ui_cards::segmented_pill(
            canvas,
            text,
            theme,
            pill,
            &cat_labels[idx],
            None,
            selected,
            hovered,
        );
        hits.push(pill, Action::Category(*category));
        cx += w + 2;
    }

    // Segmented Sort Bar (inline right if space permits, else next row)
    let mut sort_widths = [0i32; 4];
    let mut sort_total_w = 6;
    for (idx, sort) in SortMode::ALL.iter().enumerate() {
        let w = text.measure(sort.label(), 11.5, Face::Bold) as i32 + 22;
        sort_widths[idx] = w;
        sort_total_w += w + 2;
    }
    let inline_sort = area.w - SCROLLBAR_GUTTER - cat_track.w >= sort_total_w + 16;
    let sort_track = if inline_sort {
        Rect::new(
            area.x + area.w - SCROLLBAR_GUTTER - sort_total_w,
            y,
            sort_total_w,
            34,
        )
    } else {
        y += 40;
        Rect::new(area.x, y, sort_total_w, 32)
    };
    ui_cards::segmented_track(canvas, theme, sort_track);
    let mut sx = sort_track.x + 3;
    for (idx, sort) in SortMode::ALL.iter().enumerate() {
        let w = sort_widths[idx];
        let pill = Rect::new(sx, sort_track.y + 3, w, sort_track.h - 6);
        let selected = state.sort == *sort;
        let hovered = prev_hits.hover_rect(pointer.0, pointer.1, pill);
        ui_cards::segmented_pill(
            canvas,
            text,
            theme,
            pill,
            sort.label(),
            None,
            selected,
            hovered,
        );
        hits.push(pill, Action::Sort(*sort));
        sx += w + 2;
    }
    y += 46;

    let viewport = Rect::new(area.x, y, area.w, (area.bottom() - y).max(40));
    let packages: Vec<StorePackage> = state.filtered_packages().into_iter().cloned().collect();
    if packages.is_empty() {
        ui_cards::empty_state(
            canvas,
            text,
            theme,
            viewport,
            Icon::Files,
            if state.search.is_empty() {
                "没有可显示的软件包"
            } else {
                "没有匹配的软件包"
            },
            "试试更换分类、清空搜索词，或点击右上角刷新按钮。",
        );
        return;
    }

    let inner_w = (viewport.w - SCROLLBAR_GUTTER).max(160);
    let columns = ((inner_w + 16) / 310).max(1);
    let card_w = (inner_w - (columns - 1) * 16) / columns;
    let rows = (packages.len() as i32 + columns - 1) / columns;
    let total_h = (rows * (ui_cards::CARD_H + 14)).max(viewport.h);
    let sb_rect = Rect::new(viewport.right() - 8, viewport.y, 8, viewport.h);
    state
        .grid_scroll
        .set_bounds(viewport.h as f32, total_h as f32, sb_rect);
    let offset = state.grid_scroll.offset_i32();

    {
        let state_ref: &StoreState = state;
        canvas.with_clip(viewport, |canvas| {
            for (index, pkg) in packages.iter().enumerate() {
                let col = index as i32 % columns;
                let row = index as i32 / columns;
                let card = Rect::new(
                    viewport.x + col * (card_w + 16),
                    viewport.y + row * (ui_cards::CARD_H + 14) - offset,
                    card_w,
                    ui_cards::CARD_H,
                );
                if card.bottom() < viewport.y || card.y > viewport.bottom() {
                    continue;
                }
                let pres = state_ref.presentation(pkg);
                let hovered = prev_hits.hover_rect(pointer.0, pointer.1, card);
                let cta_approx = Rect::new(card.right() - 92, card.bottom() - 38, 76, 26);
                let cta_hovered = prev_hits.hover_rect(pointer.0, pointer.1, cta_approx);
                let progress = state_ref
                    .job_for(&pkg.name)
                    .map(|job| job.percent as f32 / 100.0);
                let cta = ui_cards::package_card(
                    canvas, text, theme, card, pkg, &pres, false, hovered, cta_hovered, progress,
                );
                hits.push_clipped(card, viewport, Action::OpenDetail(pkg.name.clone()));
                if state_ref.job_for(&pkg.name).is_some() {
                    hits.push_clipped(cta, viewport, Action::ToggleQueue);
                } else if pkg.upgrade_available {
                    hits.push_clipped(cta, viewport, Action::Update(pkg.name.clone()));
                } else if pkg.installed {
                    hits.push_clipped(cta, viewport, Action::OpenDetail(pkg.name.clone()));
                } else {
                    hits.push_clipped(cta, viewport, Action::Install(pkg.name.clone()));
                }
            }
        });
    }

    ui::scrollbar_v_animated(
        canvas,
        theme,
        "store_grid_scrollbar",
        sb_rect,
        total_h as f32,
        viewport.h as f32,
        offset as f32,
        state.grid_scroll.is_hovered(),
        state.grid_scroll.is_dragging(),
        state.grid_scroll.is_animating(),
    );
}

fn draw_updates(
    canvas: &mut Canvas,
    text: &mut TextRenderer,
    theme: &Theme,
    state: &mut StoreState,
    main: Rect,
    prev_hits: &Hits,
    hits: &mut Hits,
) {
    let area = content_scroll_area(main);
    let pointer = state.last_pointer;
    let inner_w = (area.w - SCROLLBAR_GUTTER).max(160);
    let mut y = area.y;
    let count = state.updates.len();

    // Elevated overview status card
    let banner = Rect::new(area.x, y, inner_w, 58);
    ui_cards::elevated_card(canvas, theme, banner, false);
    let status_accent = if count == 0 {
        [theme.success, ui_cards::mix(theme.success, 0x059669, 0.40)]
    } else {
        [theme.accent, ui_cards::mix(theme.accent, 0x1d4ed8, 0.40)]
    };
    ui_cards::icon_badge(
        canvas,
        Icon::Refresh,
        Rect::new(banner.x + 14, banner.y + 11, 36, 36),
        status_accent,
    );
    ui_cards::draw_text_clipped(
        text,
        canvas,
        &if count == 0 {
            "所有软件包均为最新版本".to_string()
        } else {
            format!("发现 {count} 个可升级软件包")
        },
        banner.x + 62,
        banner.y + 34,
        (banner.w - 300).max(80),
        14.5,
        theme.text,
        Face::Bold,
    );

    let all = Rect::new(banner.right() - 114, banner.y + 14, 100, 30);
    let hovered = prev_hits.hover_rect(pointer.0, pointer.1, all);
    ui_cards::cta_pill(
        canvas,
        text,
        theme,
        all,
        "全部更新",
        count > 0,
        hovered,
        false,
        None,
    );
    if count > 0 {
        hits.push(all, Action::UpdateAll);
    }
    let check = Rect::new(banner.right() - 220, banner.y + 14, 96, 30);
    let hovered = prev_hits.hover_rect(pointer.0, pointer.1, check);
    ui_cards::cta_pill(
        canvas,
        text,
        theme,
        check,
        "检查更新",
        false,
        hovered,
        false,
        None,
    );
    hits.push(check, Action::RefreshCatalog);
    y += 72;

    let viewport = Rect::new(area.x, y, area.w, (area.bottom() - y).max(40));
    if count == 0 {
        let empty_card = Rect::new(viewport.x, viewport.y, inner_w, (viewport.h - 8).max(160));
        ui_cards::group_container(canvas, theme, empty_card);
        ui_cards::empty_state(
            canvas,
            text,
            theme,
            empty_card,
            Icon::Refresh,
            "没有待处理的软件更新",
            "点击右上角刷新按钮或「检查更新」获取最新仓库索引。",
        );
        return;
    }

    let row_h = 68;
    let group_h = count as i32 * row_h;
    let total_h = (group_h + 12).max(viewport.h);
    let sb_rect = Rect::new(viewport.right() - 8, viewport.y, 8, viewport.h);
    state
        .grid_scroll
        .set_bounds(viewport.h as f32, total_h as f32, sb_rect);
    let offset = state.grid_scroll.offset_i32();
    let update_names = state.updates.clone();

    {
        let state_ref: &StoreState = state;
        canvas.with_clip(viewport, |canvas| {
            let group_rect = Rect::new(viewport.x, viewport.y - offset, inner_w, group_h);
            ui_cards::group_container(canvas, theme, group_rect);

            for (index, name) in update_names.iter().enumerate() {
                let Some(pkg) = state_ref.package(name) else {
                    continue;
                };
                let row = Rect::new(
                    viewport.x,
                    viewport.y + index as i32 * row_h - offset,
                    inner_w,
                    row_h,
                );
                if row.bottom() < viewport.y || row.y > viewport.bottom() {
                    continue;
                }
                let row_hovered = prev_hits.hover_rect(pointer.0, pointer.1, row);
                if row_hovered {
                    canvas.rounded_rect(
                        row.inset(3),
                        8,
                        ui_cards::mix(theme.window_high, theme.panel_hover, 0.35),
                        1.0,
                    );
                }
                if index > 0 {
                    canvas.line(
                        row.x + 70,
                        row.y,
                        row.right() - 16,
                        row.y,
                        ui_cards::soft_border(theme),
                        0.85,
                    );
                }

                let pres = state_ref.presentation(pkg);
                ui_cards::icon_badge(
                    canvas,
                    pres.icon,
                    Rect::new(row.x + 16, row.y + 13, 42, 42),
                    pres.accent,
                );
                ui_cards::draw_text_clipped(
                    text,
                    canvas,
                    &pres.display_name,
                    row.x + 70,
                    row.y + 27,
                    row.w - 310,
                    14.0,
                    theme.text,
                    Face::Bold,
                );
                let name_w = text.measure(&pres.display_name, 14.0, Face::Bold) as i32;
                let from = pkg.installed_version.as_deref().unwrap_or("-");
                let to = pkg.available_version.as_deref().unwrap_or("-");
                let ver_label = format!("{from} → {to}");
                let ver_w = (text.measure(&ver_label, 11.0, Face::Bold) as i32 + 18).min(180);
                let ver_x = row.x + 70 + name_w + 10;
                if ver_x + ver_w < row.right() - 160 {
                    ui_cards::badge(
                        canvas,
                        text,
                        theme,
                        Rect::new(ver_x, row.y + 12, ver_w, 20),
                        &ver_label,
                        theme.accent,
                    );
                }
                if pkg.held {
                    let lock_x = (ver_x + ver_w + 6).min(row.right() - 208);
                    ui_cards::badge(
                        canvas,
                        text,
                        theme,
                        Rect::new(lock_x, row.y + 12, 56, 20),
                        "已锁定",
                        theme.warning,
                    );
                }
                ui_cards::draw_text_clipped(
                    text,
                    canvas,
                    &format!("{}  ·  下载体积 {}", pres.subtitle, format_bytes(pkg.size)),
                    row.x + 70,
                    row.y + 47,
                    row.w - 230,
                    11.5,
                    theme.text_muted,
                    Face::Ui,
                );
                hits.push_clipped(row, viewport, Action::OpenDetail(pkg.name.clone()));

                let cta = Rect::new(row.right() - 134, row.y + 20, 78, 28);
                let hovered = prev_hits.hover_rect(pointer.0, pointer.1, cta);
                if let Some(job) = state_ref.job_for(&pkg.name) {
                    ui_cards::inline_progress(
                        canvas,
                        theme,
                        cta,
                        job.percent as f32 / 100.0,
                        theme.accent,
                    );
                    hits.push_clipped(cta, viewport, Action::ToggleQueue);
                } else if pkg.held {
                    ui_cards::cta_pill(
                        canvas, text, theme, cta, "解锁", false, hovered, false, None,
                    );
                    hits.push_clipped(cta, viewport, Action::Unhold(pkg.name.clone()));
                } else {
                    ui_cards::cta_pill(
                        canvas, text, theme, cta, "更新", true, hovered, false, None,
                    );
                    hits.push_clipped(cta, viewport, Action::Update(pkg.name.clone()));
                }

                let more = Rect::new(row.right() - 48, row.y + 20, 34, 28);
                let menu_open = state_ref
                    .open_menu
                    .as_ref()
                    .is_some_and(|(cur, _)| cur == &pkg.name);
                let hov_more = prev_hits.hover_rect(pointer.0, pointer.1, more);
                ui_cards::more_button(canvas, theme, more, menu_open, hov_more);
                hits.push_clipped(
                    more,
                    viewport,
                    Action::TogglePkgMenu(pkg.name.clone(), more),
                );
            }
        });
    }

    ui::scrollbar_v_animated(
        canvas,
        theme,
        "store_updates_scrollbar",
        sb_rect,
        total_h as f32,
        viewport.h as f32,
        offset as f32,
        state.grid_scroll.is_hovered(),
        state.grid_scroll.is_dragging(),
        state.grid_scroll.is_animating(),
    );
}

fn draw_installed(
    canvas: &mut Canvas,
    text: &mut TextRenderer,
    theme: &Theme,
    state: &mut StoreState,
    main: Rect,
    prev_hits: &Hits,
    hits: &mut Hits,
) {
    let area = content_scroll_area(main);
    let pointer = state.last_pointer;
    let inner_w = (area.w - SCROLLBAR_GUTTER).max(160);
    let mut y = area.y;

    let counts = [
        state.installed_count(),
        state.explicit_count(),
        state.dependency_count(),
        state.held_count(),
    ];
    let mut pill_widths = [0i32; 4];
    let mut track_w = 6;
    for (index, filter) in InstalledFilter::ALL.iter().enumerate() {
        let lw = text.measure(filter.label(), 12.0, Face::Bold) as i32;
        let cw = (text.measure(&counts[index].to_string(), 10.5, Face::Bold) as i32 + 12).max(20);
        let w = lw + cw + 26;
        pill_widths[index] = w;
        track_w += w + 2;
    }
    let track = Rect::new(area.x, y, track_w, 34);
    ui_cards::segmented_track(canvas, theme, track);
    let mut x = track.x + 3;
    for (index, filter) in InstalledFilter::ALL.iter().enumerate() {
        let w = pill_widths[index];
        let pill = Rect::new(x, track.y + 3, w, 28);
        let selected = state.installed_filter == *filter;
        let hovered = prev_hits.hover_rect(pointer.0, pointer.1, pill);
        ui_cards::segmented_pill(
            canvas,
            text,
            theme,
            pill,
            filter.label(),
            Some(counts[index]),
            selected,
            hovered,
        );
        hits.push(pill, Action::InstalledFilter(*filter));
        x += w + 2;
    }

    let packages: Vec<StorePackage> = state.filtered_packages().into_iter().cloned().collect();
    let total_installed_bytes: u64 = packages
        .iter()
        .map(|p| p.installed_size.unwrap_or(p.size))
        .sum();
    if inner_w - track.w >= 160 {
        let summary = format!(
            "当前列表 {} 项 · 总占用 {}",
            packages.len(),
            format_bytes(total_installed_bytes)
        );
        let sw = text.measure(&summary, 11.5, Face::Ui) as i32;
        ui_cards::draw_text(
            text,
            canvas,
            &summary,
            area.x + inner_w - sw - 4,
            y + 22,
            11.5,
            theme.text_muted,
            Face::Ui,
        );
    }
    y += 46;

    let viewport = Rect::new(area.x, y, area.w, (area.bottom() - y).max(40));
    if packages.is_empty() {
        let empty_card = Rect::new(viewport.x, viewport.y, inner_w, (viewport.h - 8).max(160));
        ui_cards::group_container(canvas, theme, empty_card);
        ui_cards::empty_state(
            canvas,
            text,
            theme,
            empty_card,
            Icon::Folder,
            "当前筛选下没有已安装软件包",
            "可切换上方筛选标签，或前往「探索精选」「分类浏览」安装新软件。",
        );
        return;
    }

    // Partition into GNOME Software / Mac App Store style PreferencesGroups:
    // Group 1: Desktop Applications & Themes
    // Group 2: Core System & Runtime Components
    let mut app_group: Vec<StorePackage> = Vec::new();
    let mut sys_group: Vec<StorePackage> = Vec::new();
    for pkg in packages {
        let pres = state.presentation(&pkg);
        let is_desktop_app_or_theme = pres.is_theme
            || pres.category == StoreCategory::Utilities
            || pres.category == StoreCategory::Themes
            || matches!(
                pkg.name.as_str(),
                "space-editor" | "space-store" | "space-askpass"
            );
        if is_desktop_app_or_theme {
            app_group.push(pkg);
        } else {
            sys_group.push(pkg);
        }
    }

    let mut groups: Vec<(&str, &str, Vec<StorePackage>)> = Vec::new();
    if !app_group.is_empty() {
        groups.push(("桌面应用与外观主题", "", app_group));
    }
    if !sys_group.is_empty() {
        groups.push(("系统核心与运行库", "", sys_group));
    }

    let row_h = 64;
    let hdr_h = 34;
    let group_gap = 16;
    let mut content_h = 4;
    for (_, _, items) in &groups {
        content_h += hdr_h + items.len() as i32 * row_h + group_gap;
    }
    let total_h = content_h.max(viewport.h);
    let sb_rect = Rect::new(viewport.right() - 8, viewport.y, 8, viewport.h);
    state
        .grid_scroll
        .set_bounds(viewport.h as f32, total_h as f32, sb_rect);
    let offset = state.grid_scroll.offset_i32();

    {
        let state_ref: &StoreState = state;
        canvas.with_clip(viewport, |canvas| {
            let mut cy = viewport.y - offset;
            for (g_title, g_sub, items) in &groups {
                let hdr_rect = Rect::new(viewport.x, cy, inner_w, hdr_h);
                if hdr_rect.bottom() >= viewport.y && hdr_rect.y <= viewport.bottom() {
                    ui_cards::group_section_header(
                        canvas,
                        text,
                        theme,
                        hdr_rect,
                        g_title,
                        items.len(),
                        g_sub,
                    );
                }
                cy += hdr_h;

                let group_h = items.len() as i32 * row_h;
                let group_rect = Rect::new(viewport.x, cy, inner_w, group_h);
                if group_rect.bottom() >= viewport.y && group_rect.y <= viewport.bottom() {
                    ui_cards::group_container(canvas, theme, group_rect);

                    for (index, pkg) in items.iter().enumerate() {
                        let row = Rect::new(viewport.x, cy + index as i32 * row_h, inner_w, row_h);
                        if row.bottom() < viewport.y || row.y > viewport.bottom() {
                            continue;
                        }

                        let cta_rect = Rect::new(row.right() - 134, row.y + 18, 76, 28);
                        let more_rect = Rect::new(row.right() - 48, row.y + 18, 34, 28);
                        let menu_open = state_ref
                            .open_menu
                            .as_ref()
                            .is_some_and(|(cur, _)| cur == &pkg.name);

                        let row_hovered = prev_hits.hover_rect(pointer.0, pointer.1, row)
                            || prev_hits.hover_rect(pointer.0, pointer.1, cta_rect)
                            || prev_hits.hover_rect(pointer.0, pointer.1, more_rect)
                            || menu_open;
                        if row_hovered {
                            canvas.rounded_rect(
                                row.inset(3),
                                8,
                                ui_cards::mix(theme.window_high, theme.panel_hover, 0.35),
                                1.0,
                            );
                        }
                        if index > 0 {
                            canvas.line(
                                row.x + 70,
                                row.y,
                                row.right() - 16,
                                row.y,
                                ui_cards::soft_border(theme),
                                0.85,
                            );
                        }

                        let pres = state_ref.presentation(pkg);
                        ui_cards::icon_badge(
                            canvas,
                            pres.icon,
                            Rect::new(row.x + 16, row.y + 11, 42, 42),
                            pres.accent,
                        );

                        let title_max_w = (row.w - 340).max(90);
                        ui_cards::draw_text_clipped(
                            text,
                            canvas,
                            &pres.display_name,
                            row.x + 70,
                            row.y + 26,
                            title_max_w,
                            14.0,
                            theme.text,
                            Face::Bold,
                        );

                        // Inline version & metadata badges
                        let shown_title =
                            text.truncate(&pres.display_name, title_max_w as f32, 14.0, Face::Bold);
                        let mut bx =
                            row.x + 70 + text.measure(&shown_title, 14.0, Face::Bold) as i32 + 10;
                        let max_bx = cta_rect.x - 12;

                        if let Some(ver) = pkg.installed_version.as_deref() {
                            let vw = (text.measure(ver, 11.0, Face::Bold) as i32 + 16).max(42);
                            if bx + vw <= max_bx {
                                ui_cards::badge(
                                    canvas,
                                    text,
                                    theme,
                                    Rect::new(bx, row.y + 11, vw, 19),
                                    ver,
                                    theme.panel_border,
                                );
                                bx += vw + 6;
                            }
                        }
                        if pkg.install_reason.as_deref() == Some("dependency") && bx + 62 <= max_bx
                        {
                            ui_cards::badge(
                                canvas,
                                text,
                                theme,
                                Rect::new(bx, row.y + 11, 60, 19),
                                "自动依赖",
                                theme.text_muted,
                            );
                            bx += 66;
                        }
                        if pkg.held && bx + 56 <= max_bx {
                            ui_cards::badge(
                                canvas,
                                text,
                                theme,
                                Rect::new(bx, row.y + 11, 54, 19),
                                "已锁定",
                                theme.warning,
                            );
                            bx += 60;
                        }
                        if pkg.upgrade_available && bx + 56 <= max_bx {
                            ui_cards::badge(
                                canvas,
                                text,
                                theme,
                                Rect::new(bx, row.y + 11, 54, 19),
                                "有更新",
                                theme.accent,
                            );
                        }

                        let size_bytes = pkg.installed_size.unwrap_or(pkg.size);
                        let subtitle = if size_bytes > 0 {
                            format!("{}  ·  占用 {}", pres.subtitle, format_bytes(size_bytes))
                        } else {
                            pres.subtitle.clone()
                        };
                        ui_cards::draw_text_clipped(
                            text,
                            canvas,
                            &subtitle,
                            row.x + 70,
                            row.y + 46,
                            (cta_rect.x - (row.x + 78)).max(80),
                            11.5,
                            theme.text_muted,
                            Face::Ui,
                        );

                        // Row click opens package detail
                        hits.push_clipped(row, viewport, Action::OpenDetail(pkg.name.clone()));

                        // Single Primary Action Button
                        let hov_cta = prev_hits.hover_rect(pointer.0, pointer.1, cta_rect);
                        if let Some(job) = state_ref.job_for(&pkg.name) {
                            ui_cards::inline_progress(
                                canvas,
                                theme,
                                cta_rect,
                                job.percent as f32 / 100.0,
                                theme.accent,
                            );
                            hits.push_clipped(cta_rect, viewport, Action::ToggleQueue);
                        } else if pkg.upgrade_available {
                            ui_cards::cta_pill(
                                canvas,
                                text,
                                theme,
                                cta_rect,
                                "更新",
                                true,
                                hov_cta,
                                false,
                                Some(pres.accent[0]),
                            );
                            hits.push_clipped(
                                cta_rect,
                                viewport,
                                Action::Update(pkg.name.clone()),
                            );
                        } else {
                            ui_cards::cta_pill(
                                canvas,
                                text,
                                theme,
                                cta_rect,
                                "详情",
                                false,
                                hov_cta,
                                false,
                                None,
                            );
                            hits.push_clipped(
                                cta_rect,
                                viewport,
                                Action::OpenDetail(pkg.name.clone()),
                            );
                        }

                        // `···` Overflow Menu Button
                        let hov_more = prev_hits.hover_rect(pointer.0, pointer.1, more_rect);
                        ui_cards::more_button(canvas, theme, more_rect, menu_open, hov_more);
                        hits.push_clipped(
                            more_rect,
                            viewport,
                            Action::TogglePkgMenu(pkg.name.clone(), more_rect),
                        );
                    }
                }
                cy += group_h + group_gap;
            }
        });
    }

    ui::scrollbar_v_animated(
        canvas,
        theme,
        "store_installed_scrollbar",
        sb_rect,
        total_h as f32,
        viewport.h as f32,
        offset as f32,
        state.grid_scroll.is_hovered(),
        state.grid_scroll.is_dragging(),
        state.grid_scroll.is_animating(),
    );
}

#[allow(clippy::too_many_arguments)]
fn draw_pkg_menu_overlay(
    canvas: &mut Canvas,
    text: &mut TextRenderer,
    theme: &Theme,
    state: &StoreState,
    content: Rect,
    pkg_name: &str,
    anchor: Rect,
    prev_hits: &Hits,
    hits: &mut Hits,
) {
    // Clicking anywhere outside closes the popover menu cleanly.
    hits.push(content, Action::ClosePkgMenu);

    let Some(pkg) = state.package(pkg_name) else {
        return;
    };
    let pointer = state.last_pointer;
    let menu_w = 168;
    let menu_h = 148;
    let mx = (anchor.right() - menu_w).clamp(content.x + 12, content.right() - menu_w - 12);
    let my = if anchor.bottom() + 6 + menu_h <= content.bottom() - 12 {
        anchor.bottom() + 6
    } else {
        (anchor.y - menu_h - 6).max(content.y + 12)
    };
    let menu_rect = Rect::new(mx, my, menu_w, menu_h);

    let radius = theme.radius_control().max(10);
    canvas.rounded_drop_shadow(menu_rect, radius, 18, 5, 0.24);
    canvas.rounded_rect(menu_rect, radius, theme.window_high, 1.0);
    canvas.rounded_border(menu_rect, radius, theme.window_border, 0.95, 1);
    hits.push(menu_rect, Action::Noop);

    let items: [(&str, Action, bool); 4] = [
        (
            "查看软件包详情",
            Action::OpenDetail(pkg.name.clone()),
            false,
        ),
        (
            if pkg.held {
                "解除版本锁定"
            } else {
                "锁定当前版本"
            },
            if pkg.held {
                Action::Unhold(pkg.name.clone())
            } else {
                Action::Hold(pkg.name.clone())
            },
            false,
        ),
        ("重新安装软件包", Action::Reinstall(pkg.name.clone()), false),
        ("卸载软件包…", Action::Remove(pkg.name.clone()), true),
    ];

    let mut iy = menu_rect.y + 6;
    for (idx, (label, act, is_danger)) in items.into_iter().enumerate() {
        if idx == 3 {
            canvas.line(
                menu_rect.x + 10,
                iy + 2,
                menu_rect.right() - 10,
                iy + 2,
                ui_cards::soft_border(theme),
                0.9,
            );
            iy += 6;
        }
        let item_r = Rect::new(menu_rect.x + 6, iy, menu_rect.w - 12, 32);
        let hov = prev_hits.hover_rect(pointer.0, pointer.1, item_r);
        if hov {
            let h_bg = if is_danger {
                ui_cards::mix(theme.window_high, theme.danger, 0.16)
            } else {
                ui_cards::mix(theme.window_high, theme.accent, 0.15)
            };
            canvas.rounded_rect(item_r, 6, h_bg, 1.0);
        }
        let fg = if is_danger {
            ui_cards::badge_text_color(theme, theme.danger)
        } else {
            theme.text
        };
        ui_cards::draw_text_clipped(
            text,
            canvas,
            label,
            item_r.x + 12,
            item_r.y + 21,
            item_r.w - 24,
            12.0,
            fg,
            if is_danger || hov {
                Face::Bold
            } else {
                Face::Ui
            },
        );
        hits.push(item_r, act);
        iy += 33;
    }
}

fn draw_detail(
    canvas: &mut Canvas,
    text: &mut TextRenderer,
    theme: &Theme,
    state: &mut StoreState,
    main: Rect,
    prev_hits: &Hits,
    hits: &mut Hits,
) {
    let Some(name) = state.detail.clone() else {
        return;
    };
    let Some(pkg) = state.package(&name).cloned() else {
        canvas.fill(main, theme.window);
        ui_cards::empty_state(
            canvas,
            text,
            theme,
            main,
            Icon::Files,
            "找不到这个软件包",
            "它可能已经从软件源中移除。",
        );
        return;
    };
    canvas.fill(main, theme.window);
    let pres = state.presentation(&pkg);
    let area = content_scroll_area(main);
    let header_h = 122;
    let header = Rect::new(area.x, area.y, area.w, header_h);
    ui_cards::elevated_card(canvas, theme, header, false);

    let show_preview =
        pres.screenshots.is_empty() && (pres.banner.is_some() || pres.is_theme) && header.w >= 720;
    let preview_w = if show_preview { 220 } else { 0 };
    if show_preview {
        let pr = Rect::new(header.right() - preview_w - 16, header.y + 10, preview_w, 102);
        let banner = state.banner_sprite(pres.banner.as_deref());
        ui_cards::draw_preview_artwork(
            canvas,
            pr,
            8,
            pres.accent,
            pres.icon,
            banner,
            pres.is_theme,
        );
    }

    ui_cards::icon_badge(
        canvas,
        pres.icon,
        Rect::new(header.x + 18, header.y + 18, 64, 64),
        pres.accent,
    );
    let info_x = header.x + 96;
    let info_w = (header.w - 112 - if show_preview { preview_w + 24 } else { 0 }).max(120);
    ui_cards::draw_text_clipped(
        text,
        canvas,
        &pres.display_name,
        info_x,
        header.y + 36,
        info_w,
        19.0,
        theme.text,
        Face::Bold,
    );
    ui_cards::draw_text_clipped(
        text,
        canvas,
        &pres.subtitle,
        info_x,
        header.y + 58,
        info_w,
        12.5,
        theme.text_muted,
        Face::Ui,
    );

    let pointer = state.last_pointer;
    let mut button_x = info_x;
    let btn_y = header.y + 76;
    let primary_rect = Rect::new(button_x, btn_y, 114, 32);
    let hovered = prev_hits.hover_rect(pointer.0, pointer.1, primary_rect);
    if let Some(job) = state.job_for(&name) {
        ui_cards::inline_progress(
            canvas,
            theme,
            primary_rect,
            job.percent as f32 / 100.0,
            theme.accent,
        );
        hits.push(primary_rect, Action::ToggleQueue);
    } else if pkg.upgrade_available {
        ui_cards::cta_pill(
            canvas,
            text,
            theme,
            primary_rect,
            "更新",
            true,
            hovered,
            false,
            Some(pres.accent[0]),
        );
        hits.push(primary_rect, Action::Update(name.clone()));
    } else if pkg.installed {
        ui_cards::cta_pill(
            canvas,
            text,
            theme,
            primary_rect,
            "重新安装",
            false,
            hovered,
            false,
            None,
        );
        hits.push(primary_rect, Action::Reinstall(name.clone()));
    } else {
        ui_cards::cta_pill(
            canvas,
            text,
            theme,
            primary_rect,
            "安装",
            true,
            hovered,
            false,
            Some(pres.accent[0]),
        );
        hits.push(primary_rect, Action::Install(name.clone()));
    }
    button_x += 124;

    if pkg.installed {
        let remove = Rect::new(button_x, btn_y, 86, 32);
        let hovered = prev_hits.hover_rect(pointer.0, pointer.1, remove);
        ui_cards::danger_pill(canvas, text, theme, remove, "卸载", hovered);
        hits.push(remove, Action::Remove(name.clone()));
        button_x += 96;

        let hold = Rect::new(button_x, btn_y, 92, 32);
        let hovered = prev_hits.hover_rect(pointer.0, pointer.1, hold);
        ui_cards::cta_pill(
            canvas,
            text,
            theme,
            hold,
            if pkg.held { "解除锁定" } else { "锁定版本" },
            false,
            hovered,
            false,
            None,
        );
        hits.push(
            hold,
            if pkg.held {
                Action::Unhold(name.clone())
            } else {
                Action::Hold(name.clone())
            },
        );
        button_x += 102;
    }
    if pres.is_theme && button_x + 120 <= header.right() - if show_preview { preview_w + 24 } else { 16 } {
        let appearance = Rect::new(button_x, btn_y, 124, 32);
        let hovered = prev_hits.hover_rect(pointer.0, pointer.1, appearance);
        ui_cards::cta_pill(
            canvas,
            text,
            theme,
            appearance,
            "去外观设置切换",
            false,
            hovered,
            false,
            None,
        );
        hits.push(appearance, Action::OpenAppearance);
    }

    let stats = Rect::new(area.x, header.bottom() + 10, area.w, 56);
    let source_label = pkg.repo.clone().unwrap_or_else(|| {
        if pkg.installed {
            "本地已安装".into()
        } else {
            "-".into()
        }
    });
    let items: Vec<(&str, String)> = vec![
        ("版本", pkg.display_version().to_string()),
        ("体积", format_bytes(pkg.size)),
        (
            "安装大小",
            pkg.installed_size
                .map(format_bytes)
                .unwrap_or_else(|| "-".into()),
        ),
        (
            "文件数",
            pkg.file_count
                .map(|n| n.to_string())
                .unwrap_or_else(|| "-".into()),
        ),
        ("来源", source_label),
    ];
    ui_cards::quick_stats(canvas, text, theme, stats, &items);

    let tab_defs = [
        (crate::model::DetailTab::Overview, "软件概览", None),
        (
            crate::model::DetailTab::Files,
            "文件列表",
            if state.detail_files.is_empty() {
                None
            } else {
                Some(state.detail_files.len())
            },
        ),
    ];
    let mut tab_widths = [0i32; 2];
    let mut track_w = 6;
    for (idx, (_, label, cnt)) in tab_defs.iter().enumerate() {
        let lw = text.measure(label, 12.0, Face::Bold) as i32;
        let cw = cnt
            .map(|c| (text.measure(&c.to_string(), 10.5, Face::Bold) as i32 + 12).max(20) + 6)
            .unwrap_or(0);
        let w = lw + cw + 26;
        tab_widths[idx] = w;
        track_w += w + 2;
    }
    let tabs = Rect::new(area.x, stats.bottom() + 10, track_w, 34);
    ui_cards::segmented_track(canvas, theme, tabs);
    let mut tab_x = tabs.x + 3;
    for (idx, (tab, label, cnt)) in tab_defs.into_iter().enumerate() {
        let width = tab_widths[idx];
        let rect = Rect::new(tab_x, tabs.y + 3, width, 28);
        let active = state.detail_tab == tab;
        let hovered = prev_hits.hover_rect(pointer.0, pointer.1, rect);
        ui_cards::segmented_pill(canvas, text, theme, rect, label, cnt, active, hovered);
        hits.push(rect, Action::SetTab(tab));
        tab_x += width + 2;
    }

    let body = Rect::new(
        area.x,
        tabs.bottom() + 10,
        area.w,
        (area.bottom() - tabs.bottom() - 10).max(40),
    );
    let body_card = Rect::new(
        body.x,
        body.y,
        (body.w - SCROLLBAR_GUTTER).max(160),
        body.h,
    );
    ui_cards::group_container(canvas, theme, body_card);
    let inner_body = body_card.inset(14);
    let inner_w = inner_body.w.max(120);
    let offset = state.detail_scroll.offset_i32();
    let total_h: i32;

    {
        let state_ref: &StoreState = state;
        let body = inner_body;
        let mut y = body.y - offset;
        canvas.with_clip(body, |canvas| match state_ref.detail_tab {
            crate::model::DetailTab::Overview => {
                if !pres.screenshots.is_empty() {
                    let shot_count = pres.screenshots.len();
                    let active_idx = state_ref.detail_shot_idx % shot_count;
                    ui_cards::draw_text(
                        text,
                        canvas,
                        "实机截图",
                        body.x,
                        y + 14,
                        13.5,
                        theme.text,
                        Face::Bold,
                    );
                    if shot_count > 1 {
                        let count_str = format!("{} / {}", active_idx + 1, shot_count);
                        ui_cards::draw_text(
                            text,
                            canvas,
                            &count_str,
                            body.x + 68,
                            y + 14,
                            11.5,
                            theme.text_muted,
                            Face::Ui,
                        );
                    }
                    y += 22;

                    let side_by_side = inner_w >= 560 && shot_count > 1;
                    let (gallery_w, gallery_h) = if side_by_side {
                        (376, 212)
                    } else {
                        let gw = inner_w.min(480);
                        (gw, (gw * 9 / 16).clamp(160, 240))
                    };
                    let gallery_rect = Rect::new(body.x, y, gallery_w, gallery_h);
                    let active_rel = pres.screenshots.get(active_idx).map(String::as_str);
                    let sprite = state_ref.banner_sprite(active_rel);
                    ui_cards::draw_preview_artwork(
                        canvas,
                        gallery_rect,
                        10,
                        pres.accent,
                        pres.icon,
                        sprite,
                        pres.is_theme,
                    );
                    if shot_count > 1 {
                        let prev_btn = Rect::new(
                            gallery_rect.x + 10,
                            gallery_rect.y + (gallery_h - 32) / 2,
                            28,
                            32,
                        );
                        let next_btn = Rect::new(
                            gallery_rect.right() - 38,
                            gallery_rect.y + (gallery_h - 32) / 2,
                            28,
                            32,
                        );
                        for (btn_rect, lbl, act) in [
                            (prev_btn, "‹", Action::DetailShotPrev),
                            (next_btn, "›", Action::DetailShotNext),
                        ] {
                            let hov = prev_hits.hover_rect(pointer.0, pointer.1, btn_rect);
                            canvas.rounded_rect(
                                btn_rect,
                                14,
                                0x0f172a,
                                if hov { 0.72 } else { 0.45 },
                            );
                            let tw = text.measure(lbl, 16.0, Face::Bold);
                            text.draw(
                                canvas,
                                lbl,
                                (btn_rect.x + btn_rect.w / 2) as f32 - tw * 0.5,
                                (btn_rect.y + 21) as f32,
                                16.0,
                                0xffffff,
                                Face::Bold,
                            );
                            hits.push_clipped(btn_rect, body, act);
                        }

                        if side_by_side {
                            let thumb_w = 144;
                            let thumb_h = 81;
                            let gap = 10;
                            let start_tx = gallery_rect.right() + 14;
                            let mut tx = start_tx;
                            let mut ty = y;
                            for (idx, shot_rel) in pres.screenshots.iter().enumerate() {
                                if ty + thumb_h > y + gallery_h + 4 {
                                    tx += thumb_w + gap;
                                    ty = y;
                                }
                                if tx + thumb_w > body.x + inner_w {
                                    break;
                                }
                                let tr = Rect::new(tx, ty, thumb_w, thumb_h);
                                let t_sprite = state_ref.banner_sprite(Some(shot_rel.as_str()));
                                ui_cards::draw_preview_artwork(
                                    canvas,
                                    tr,
                                    8,
                                    pres.accent,
                                    pres.icon,
                                    t_sprite,
                                    pres.is_theme,
                                );
                                let selected = idx == active_idx;
                                let hov = prev_hits.hover_rect(pointer.0, pointer.1, tr);
                                if selected || hov {
                                    canvas.rounded_border(
                                        tr,
                                        8,
                                        if selected { theme.accent } else { theme.text },
                                        0.95,
                                        if selected { 2 } else { 1 },
                                    );
                                }
                                hits.push_clipped(tr, body, Action::SelectDetailShot(idx));
                                ty += thumb_h + gap;
                            }
                            y += gallery_h + 14;
                        } else {
                            y += gallery_h + 10;
                            let thumb_w = 128;
                            let thumb_h = 72;
                            let mut tx = body.x;
                            for (idx, shot_rel) in pres.screenshots.iter().enumerate() {
                                if tx + thumb_w > body.x + inner_w {
                                    break;
                                }
                                let tr = Rect::new(tx, y, thumb_w, thumb_h);
                                let t_sprite = state_ref.banner_sprite(Some(shot_rel.as_str()));
                                ui_cards::draw_preview_artwork(
                                    canvas,
                                    tr,
                                    7,
                                    pres.accent,
                                    pres.icon,
                                    t_sprite,
                                    pres.is_theme,
                                );
                                let selected = idx == active_idx;
                                let hov = prev_hits.hover_rect(pointer.0, pointer.1, tr);
                                if selected || hov {
                                    canvas.rounded_border(
                                        tr,
                                        7,
                                        if selected { theme.accent } else { theme.text },
                                        0.95,
                                        if selected { 2 } else { 1 },
                                    );
                                }
                                hits.push_clipped(tr, body, Action::SelectDetailShot(idx));
                                tx += thumb_w + 10;
                            }
                            y += thumb_h + 14;
                        }
                    } else {
                        y += gallery_h + 14;
                    }
                }
                for line in ui_cards::wrap_text(
                    text,
                    &pres.description,
                    inner_w as f32,
                    12.5,
                    Face::Ui,
                    24,
                ) {
                    ui_cards::draw_text_clipped(
                        text, canvas, &line, body.x, y + 14, inner_w, 12.5, theme.text, Face::Ui,
                    );
                    y += 20;
                }
                if !pres.highlights.is_empty() {
                    y += 10;
                    ui_cards::draw_text(
                        text,
                        canvas,
                        "亮点",
                        body.x,
                        y + 14,
                        13.5,
                        theme.text,
                        Face::Bold,
                    );
                    y += 26;
                    for highlight in &pres.highlights {
                        canvas.rounded_rect(
                            Rect::new(body.x, y + 7, 5, 5),
                            2,
                            theme.accent,
                            1.0,
                        );
                        let lines = ui_cards::wrap_text(
                            text,
                            highlight,
                            (inner_w - 18) as f32,
                            12.0,
                            Face::Ui,
                            3,
                        );
                        for (index, line) in lines.iter().enumerate() {
                            ui_cards::draw_text_clipped(
                                text,
                                canvas,
                                line,
                                body.x + 16,
                                y + 14 + index as i32 * 18,
                                inner_w - 18,
                                12.0,
                                theme.text_muted,
                                Face::Ui,
                            );
                        }
                        y += (lines.len().max(1) as i32) * 18 + 8;
                    }
                }
                if !pkg.depends.is_empty() {
                    y += 8;
                    ui_cards::draw_text(
                        text,
                        canvas,
                        "依赖",
                        body.x,
                        y + 14,
                        13.5,
                        theme.text,
                        Face::Bold,
                    );
                    y += 26;
                    let mut dx = body.x;
                    for dep in &pkg.depends {
                        let dep_name = dep
                            .split(|c: char| c == '>' || c == '<' || c == '=')
                            .next()
                            .unwrap_or(dep);
                        let exists = state_ref.package(dep_name).is_some();
                        let width = (text.measure(dep, 12.0, Face::Ui) as i32 + 20).max(64);
                        if dx > body.x && dx + width > body.x + inner_w {
                            dx = body.x;
                            y += 30;
                        }
                        let pill = Rect::new(dx, y, width.min(inner_w), 24);
                        ui_cards::badge(
                            canvas,
                            text,
                            theme,
                            pill,
                            dep,
                            if exists {
                                theme.accent
                            } else {
                                theme.panel_border
                            },
                        );
                        if exists {
                            hits.push_clipped(pill, body, Action::OpenDetail(dep_name.to_string()));
                        }
                        dx += pill.w + 8;
                    }
                    y += 32;
                }
            }
            crate::model::DetailTab::Files => {
                if state_ref.detail_files.is_empty() {
                    ui_cards::draw_text(
                        text,
                        canvas,
                        "尚未读取文件列表（仅已安装的软件包提供）。",
                        body.x,
                        y + 16,
                        12.5,
                        theme.text_muted,
                        Face::Ui,
                    );
                    y += 28;
                } else {
                    for file in &state_ref.detail_files {
                        if y + 20 >= body.y && y <= body.bottom() {
                            ui_cards::draw_text_clipped(
                                text,
                                canvas,
                                &file.path,
                                body.x,
                                y + 14,
                                inner_w - 132,
                                11.5,
                                theme.text,
                                Face::Mono,
                            );
                            ui_cards::draw_text_clipped(
                                text,
                                canvas,
                                &format!("{} · {}", file.file_type, format_bytes(file.size)),
                                body.x + inner_w - 124,
                                y + 14,
                                124,
                                11.0,
                                theme.text_muted,
                                Face::Ui,
                            );
                        }
                        y += 20;
                    }
                }
            }
        });
        total_h = (y + offset - body.y + 8).max(body.h);
    }

    let sb_rect = Rect::new(body.right() - 8, body_card.y, 8, body_card.h);
    state
        .detail_scroll
        .set_bounds(inner_body.h as f32, total_h as f32, sb_rect);
    ui::scrollbar_v_animated(
        canvas,
        theme,
        "store_detail_scrollbar",
        sb_rect,
        total_h as f32,
        inner_body.h as f32,
        offset as f32,
        state.detail_scroll.is_hovered(),
        state.detail_scroll.is_dragging(),
        state.detail_scroll.is_animating(),
    );
}

fn draw_maintenance(
    canvas: &mut Canvas,
    text: &mut TextRenderer,
    theme: &Theme,
    state: &mut StoreState,
    main: Rect,
    prev_hits: &Hits,
    hits: &mut Hits,
) -> Option<Rect> {
    let viewport = content_scroll_area(main);
    let inner_w = (viewport.w - SCROLLBAR_GUTTER).max(160);
    let offset = state.maint_scroll.offset_i32();
    let pointer = state.last_pointer;
    let total_h: i32;
    let mut active_caret: Option<Rect> = None;

    {
        let state_ref: &StoreState = state;
        let focus = state_ref.active_focus();
        let mut y = viewport.y - offset;

        canvas.with_clip(viewport, |canvas| {
            ui_cards::section_header(
                canvas,
                text,
                theme,
                Rect::new(viewport.x, y, inner_w, 28),
                "软件源",
                "",
            );
            y += 32;

            for repo in &state_ref.repos {
                let row = Rect::new(viewport.x, y, inner_w, 56);
                ui_cards::elevated_card(canvas, theme, row, false);
                ui_cards::draw_text_clipped(
                    text,
                    canvas,
                    &repo.name,
                    row.x + 14,
                    row.y + 22,
                    row.w - 300,
                    13.0,
                    theme.text,
                    Face::Bold,
                );
                let sync_ts = state_ref
                    .last_sync_at
                    .unwrap_or(0)
                    .max(repo.last_sync)
                    .max(repo.generated_at);
                ui_cards::draw_text_clipped(
                    text,
                    canvas,
                    &format!(
                        "优先级 {} · {} · {} 个包 · {}",
                        repo.priority,
                        repo.arch,
                        repo.package_count,
                        format_timestamp(sync_ts)
                    ),
                    row.x + 14,
                    row.y + 42,
                    row.w - 300,
                    11.0,
                    theme.text_muted,
                    Face::Ui,
                );
                let (st_label, st_color) = if !repo.enabled {
                    ("已禁用", theme.text_muted)
                } else if state_ref.offline || repo.stale || !repo.synced {
                    ("离线缓存", theme.warning)
                } else {
                    ("在线", theme.success)
                };
                ui_cards::badge(
                    canvas,
                    text,
                    theme,
                    Rect::new(row.right() - 270, row.y + 17, 70, 22),
                    st_label,
                    st_color,
                );
                let up = Rect::new(row.right() - 188, row.y + 14, 28, 28);
                let hovered = prev_hits.hover_rect(pointer.0, pointer.1, up);
                ui_cards::cta_pill(canvas, text, theme, up, "+", false, hovered, false, None);
                hits.push_clipped(up, viewport, Action::RepoPriority(repo.name.clone(), 1));

                let down = Rect::new(row.right() - 154, row.y + 14, 28, 28);
                let hovered = prev_hits.hover_rect(pointer.0, pointer.1, down);
                ui_cards::cta_pill(canvas, text, theme, down, "−", false, hovered, false, None);
                hits.push_clipped(down, viewport, Action::RepoPriority(repo.name.clone(), -1));

                let remove = Rect::new(row.right() - 116, row.y + 14, 100, 28);
                let hovered = prev_hits.hover_rect(pointer.0, pointer.1, remove);
                ui_cards::cta_pill(canvas, text, theme, remove, "移除", false, hovered, false, None);
                hits.push_clipped(remove, viewport, Action::RepoRemove(repo.name.clone()));
                y += 64;
            }

            let form = Rect::new(viewport.x, y, inner_w, 60);
            ui_cards::elevated_card(canvas, theme, form, false);
            let name_rect = Rect::new(form.x + 14, form.y + 14, 176, 32);
            let name_focus = focus == FocusedInput::RepoName;
            let name_caret = ui::text_input_ime(
                canvas,
                text,
                theme,
                name_rect,
                &state_ref.repo_name,
                if name_focus { &state_ref.preedit } else { "" },
                "软件源名称",
                name_focus,
                None,
            );
            hits.push_clipped(name_rect, viewport, Action::FocusInput(FocusedInput::RepoName));
            if name_focus && !name_rect.intersect(viewport).is_empty() {
                active_caret = Some(name_caret);
            }

            let url_rect = Rect::new(form.x + 200, form.y + 14, (form.w - 332).max(120), 32);
            let url_focus = focus == FocusedInput::RepoUrl;
            let url_caret = ui::text_input_ime(
                canvas,
                text,
                theme,
                url_rect,
                &state_ref.repo_url,
                if url_focus { &state_ref.preedit } else { "" },
                "https://…",
                url_focus,
                None,
            );
            hits.push_clipped(url_rect, viewport, Action::FocusInput(FocusedInput::RepoUrl));
            if url_focus && !url_rect.intersect(viewport).is_empty() {
                active_caret = Some(url_caret);
            }

            let add = Rect::new(form.right() - 118, form.y + 15, 104, 30);
            let hovered = prev_hits.hover_rect(pointer.0, pointer.1, add);
            ui_cards::cta_pill(
                canvas, text, theme, add, "添加软件源", true, hovered, false, None,
            );
            hits.push_clipped(add, viewport, Action::RepoAddSubmit);
            y += 74;

            ui_cards::section_header(
                canvas,
                text,
                theme,
                Rect::new(viewport.x, y, inner_w, 28),
                "信任密钥",
                "",
            );
            y += 32;
            for key in &state_ref.trust_keys {
                let row = Rect::new(viewport.x, y, inner_w, 44);
                ui_cards::elevated_card(canvas, theme, row, false);
                icons::draw(canvas, Icon::Shield, row.x + 14, row.y + 14, 16, Some(theme.success));
                ui_cards::draw_text_clipped(
                    text,
                    canvas,
                    &key.key_id,
                    row.x + 38,
                    row.y + 26,
                    row.w - 220,
                    12.0,
                    theme.text,
                    Face::Mono,
                );
                ui_cards::draw_text_clipped(
                    text,
                    canvas,
                    &key.comment,
                    row.right() - 180,
                    row.y + 26,
                    168,
                    11.0,
                    theme.text_muted,
                    Face::Ui,
                );
                y += 50;
            }
            let trust_row = Rect::new(viewport.x, y, inner_w, 48);
            ui_cards::elevated_card(canvas, theme, trust_row, false);
            let trust_rect = Rect::new(
                trust_row.x + 12,
                trust_row.y + 8,
                (trust_row.w - 168).max(160),
                32,
            );
            let trust_focus = focus == FocusedInput::TrustPath;
            let trust_caret = ui::text_input_ime(
                canvas,
                text,
                theme,
                trust_rect,
                &state_ref.trust_path,
                if trust_focus { &state_ref.preedit } else { "" },
                "公钥文件或目录路径（例如 /etc/spk/trusted.d/repo.pub）",
                trust_focus,
                None,
            );
            hits.push_clipped(
                trust_rect,
                viewport,
                Action::FocusInput(FocusedInput::TrustPath),
            );
            if trust_focus && !trust_rect.intersect(viewport).is_empty() {
                active_caret = Some(trust_caret);
            }
            let trust_add = Rect::new(trust_row.right() - 144, trust_row.y + 9, 130, 30);
            let hovered = prev_hits.hover_rect(pointer.0, pointer.1, trust_add);
            ui_cards::cta_pill(
                canvas,
                text,
                theme,
                trust_add,
                "导入信任密钥",
                false,
                hovered,
                false,
                None,
            );
            hits.push_clipped(trust_add, viewport, Action::TrustAdd);
            y += 62;

            ui_cards::section_header(
                canvas,
                text,
                theme,
                Rect::new(viewport.x, y, inner_w, 28),
                "缓存与一致性",
                "",
            );
            y += 32;
            let stats = Rect::new(viewport.x, y, inner_w, 64);
            let cache_items: Vec<(&str, String)> = vec![
                ("仓库索引", format_bytes(state_ref.cache.repository_bytes)),
                ("软件包", format_bytes(state_ref.cache.package_bytes)),
                ("缓存包数", state_ref.cache.package_count.to_string()),
                ("快照数", state_ref.cache.snapshot_count.to_string()),
                ("临时分片", format_bytes(state_ref.cache.part_bytes)),
            ];
            ui_cards::quick_stats(canvas, text, theme, stats, &cache_items);
            y += 76;

            let actions = [
                ("检查一致性", Action::MaintCheck),
                ("校验仓库", Action::MaintVerify),
                ("恢复事务", Action::MaintRepair),
                ("清理缓存", Action::CacheClean),
            ];
            let mut x = viewport.x;
            for (label, action) in actions {
                let width = text.measure(label, 12.0, Face::Bold) as i32 + 32;
                let rect = Rect::new(x, y, width, 30);
                let hovered = prev_hits.hover_rect(pointer.0, pointer.1, rect);
                ui_cards::cta_pill(canvas, text, theme, rect, label, false, hovered, false, None);
                hits.push_clipped(rect, viewport, action);
                x += width + 10;
            }
            y += 46;

            if let Some(result) = state_ref.maint.as_ref() {
                let detail_count = result.details.len().min(6) as i32;
                let panel_h = 74 + detail_count * 18;
                let panel = Rect::new(viewport.x, y, inner_w, panel_h);
                ui_cards::elevated_card(canvas, theme, panel, false);
                ui_cards::draw_text_clipped(
                    text,
                    canvas,
                    &result.title,
                    panel.x + 14,
                    panel.y + 24,
                    panel.w - 110,
                    13.5,
                    theme.text,
                    Face::Bold,
                );
                ui_cards::badge(
                    canvas,
                    text,
                    theme,
                    Rect::new(panel.right() - 80, panel.y + 12, 64, 22),
                    if result.ok { "通过" } else { "异常" },
                    if result.ok { theme.success } else { theme.danger },
                );
                ui_cards::draw_text_clipped(
                    text,
                    canvas,
                    &result.summary,
                    panel.x + 14,
                    panel.y + 46,
                    panel.w - 28,
                    11.5,
                    theme.text_muted,
                    Face::Ui,
                );
                let mut line_y = panel.y + 68;
                for line in result.details.iter().take(6) {
                    ui_cards::draw_text_clipped(
                        text,
                        canvas,
                        line,
                        panel.x + 14,
                        line_y,
                        panel.w - 28,
                        11.0,
                        theme.text_muted,
                        Face::Mono,
                    );
                    line_y += 18;
                }
                y += panel_h + 12;
            }
        });
        total_h = (y + offset - viewport.y + 8).max(viewport.h);
    }

    let sb_rect = Rect::new(viewport.right() - 8, viewport.y, 8, viewport.h);
    state
        .maint_scroll
        .set_bounds(viewport.h as f32, total_h as f32, sb_rect);
    ui::scrollbar_v_animated(
        canvas,
        theme,
        "store_maint_scrollbar",
        sb_rect,
        total_h as f32,
        viewport.h as f32,
        offset as f32,
        state.maint_scroll.is_hovered(),
        state.maint_scroll.is_dragging(),
        state.maint_scroll.is_animating(),
    );
    active_caret
}

fn draw_queue(
    canvas: &mut Canvas,
    text: &mut TextRenderer,
    theme: &Theme,
    state: &mut StoreState,
    rect: Rect,
    prev_hits: &Hits,
    hits: &mut Hits,
) {
    canvas.fill(rect, theme.panel);
    canvas.line(rect.x, rect.y, rect.right(), rect.y, theme.panel_border, 1.0);
    let pointer = state.last_pointer;
    let mut x = rect.x + 16;
    for (tab, label) in [
        (crate::model::QueueTab::Jobs, "任务"),
        (crate::model::QueueTab::Console, "控制台"),
    ] {
        let width = text.measure(label, 12.5, Face::Bold) as i32 + 28;
        let tab_rect = Rect::new(x, rect.y + 8, width, 28);
        let active = state.queue_tab == tab;
        let hovered = prev_hits.hover_rect(pointer.0, pointer.1, tab_rect);
        let color = ui::tab_item(canvas, theme, tab_rect, active, hovered, false, None);
        let tw = text.measure(label, 12.5, Face::Bold);
        text.draw(
            canvas,
            label,
            (tab_rect.x + tab_rect.w / 2) as f32 - tw / 2.0,
            (tab_rect.y + 18) as f32,
            12.5,
            color,
            if active { Face::Bold } else { Face::Ui },
        );
        hits.push(tab_rect, Action::QueueTab(tab));
        x += width + 6;
    }
    let close = Rect::new(rect.right() - 74, rect.y + 8, 58, 26);
    let hovered = prev_hits.hover_rect(pointer.0, pointer.1, close);
    ui_cards::cta_pill(canvas, text, theme, close, "收起", false, hovered, false, None);
    hits.push(close, Action::ToggleQueue);

    let body = Rect::new(rect.x + 16, rect.y + 42, rect.w - 32, rect.h - 50);
    let inner_w = (body.w - SCROLLBAR_GUTTER).max(160);
    let offset = state.queue_scroll.offset_i32();
    let total_h: i32;

    {
        let state_ref: &StoreState = state;
        let mut y = body.y - offset;
        canvas.with_clip(body, |canvas| match state_ref.queue_tab {
            crate::model::QueueTab::Jobs => {
                if state_ref.jobs.is_empty() {
                    ui_cards::draw_text(
                        text,
                        canvas,
                        "还没有任务。安装、更新或维护操作会出现在这里。",
                        body.x,
                        y + 18,
                        12.0,
                        theme.text_muted,
                        Face::Ui,
                    );
                    y += 30;
                } else {
                    for job in state_ref.jobs.iter().rev() {
                        let row = Rect::new(body.x, y, inner_w, 36);
                        if row.bottom() >= body.y && row.y <= body.bottom() {
                            ui_cards::draw_text_clipped(
                                text,
                                canvas,
                                &format!("#{} {}", job.id, job.title),
                                row.x,
                                row.y + 14,
                                row.w - 310,
                                12.0,
                                theme.text,
                                Face::Bold,
                            );
                            ui_cards::draw_text_clipped(
                                text,
                                canvas,
                                &format!("{} · {}", job.status.label(), job.phase_msg),
                                row.x,
                                row.y + 30,
                                row.w - 310,
                                11.0,
                                theme.text_muted,
                                Face::Ui,
                            );
                            if job.status.is_active() {
                                ui::progress_bar(
                                    canvas,
                                    theme,
                                    Rect::new(row.right() - 296, row.y + 14, 176, 8),
                                    job.percent as f32 / 100.0,
                                    Some(theme.accent),
                                );
                                if job.status == TxJobStatus::Queued {
                                    let cancel = Rect::new(row.right() - 104, row.y + 5, 92, 24);
                                    let hovered = prev_hits.hover_rect(pointer.0, pointer.1, cancel);
                                    ui_cards::cta_pill(
                                        canvas, text, theme, cancel, "取消", false, hovered, false,
                                        None,
                                    );
                                    hits.push_clipped(cancel, body, Action::CancelJob(job.id));
                                }
                            } else {
                                ui_cards::badge(
                                    canvas,
                                    text,
                                    theme,
                                    Rect::new(row.right() - 92, row.y + 7, 84, 22),
                                    job.status.label(),
                                    match job.status {
                                        TxJobStatus::Succeeded => theme.success,
                                        TxJobStatus::Failed(_) => theme.danger,
                                        _ => theme.text_muted,
                                    },
                                );
                            }
                        }
                        y += 40;
                    }
                }
            }
            crate::model::QueueTab::Console => {
                if state_ref.logs.is_empty() {
                    ui_cards::draw_text(
                        text,
                        canvas,
                        "暂无日志输出。",
                        body.x,
                        y + 18,
                        12.0,
                        theme.text_muted,
                        Face::Ui,
                    );
                    y += 28;
                } else {
                    for entry in &state_ref.logs {
                        if y + 18 >= body.y && y <= body.bottom() {
                            let color = match entry.level {
                                LogLevel::Error => theme.danger,
                                LogLevel::Warn => theme.warning,
                                LogLevel::Success => theme.success,
                                LogLevel::Phase => theme.accent,
                                LogLevel::Info => theme.text_muted,
                            };
                            ui_cards::draw_text_clipped(
                                text,
                                canvas,
                                &entry.text,
                                body.x,
                                y + 13,
                                inner_w,
                                11.0,
                                color,
                                Face::Mono,
                            );
                        }
                        y += 18;
                    }
                }
            }
        });
        total_h = (y + offset - body.y).max(body.h);
    }

    let sb_rect = Rect::new(body.right() - 8, body.y, 8, body.h);
    state
        .queue_scroll
        .set_bounds(body.h as f32, total_h as f32, sb_rect);
    ui::scrollbar_v_animated(
        canvas,
        theme,
        "store_queue_scrollbar",
        sb_rect,
        total_h as f32,
        body.h as f32,
        offset as f32,
        state.queue_scroll.is_hovered(),
        state.queue_scroll.is_dragging(),
        state.queue_scroll.is_animating(),
    );
}

#[allow(clippy::too_many_arguments)]
fn draw_preview(
    canvas: &mut Canvas,
    text: &mut TextRenderer,
    theme: &Theme,
    content: Rect,
    preview: &PreviewState,
    prev_hits: &Hits,
    hits: &mut Hits,
    pointer: (i32, i32),
) {
    let card = Rect::new(
        content.x + (content.w - 540) / 2,
        content.y + (content.h - 376) / 2,
        540,
        376,
    );
    ui_cards::modal_frame(canvas, theme, content, card);
    hits.push(content, Action::Noop);

    ui_cards::modal_title(
        canvas,
        text,
        theme,
        Rect::new(card.x + 22, card.y + 14, card.w - 44, 62),
        "确认事务",
        &preview.title,
    );
    canvas.line(card.x + 20, card.y + 78, card.right() - 20, card.y + 78, theme.panel_border, 0.7);

    let list_clip = Rect::new(card.x + 22, card.y + 86, card.w - 44, 152);
    let mut y = list_clip.y;
    canvas.with_clip(list_clip, |canvas| match &preview.plan {
        Ok(plan) => {
            if plan.nothing_to_do {
                ui_cards::draw_text(
                    text,
                    canvas,
                    "没有需要执行的操作。",
                    list_clip.x,
                    y + 14,
                    12.5,
                    theme.text_muted,
                    Face::Ui,
                );
                y += 24;
            }
            for action in plan.packages.iter().take(4) {
                let label = match action.from.as_deref() {
                    Some(from) => format!("{}  {} → {}", action.name, from, action.to),
                    None => format!("{}  {}", action.name, action.to),
                };
                ui_cards::draw_text_clipped(
                    text,
                    canvas,
                    &label,
                    list_clip.x,
                    y + 15,
                    list_clip.w - 110,
                    12.5,
                    theme.text,
                    Face::Ui,
                );
                ui_cards::draw_text_clipped(
                    text,
                    canvas,
                    &format_bytes(action.size),
                    list_clip.right() - 96,
                    y + 15,
                    96,
                    11.5,
                    theme.text_muted,
                    Face::Ui,
                );
                y += 24;
            }
            for removal in plan.removals.iter().take(3) {
                ui_cards::draw_text_clipped(
                    text,
                    canvas,
                    &format!("移除 {} {}", removal.name, removal.version),
                    list_clip.x,
                    y + 15,
                    list_clip.w,
                    12.5,
                    theme.danger,
                    Face::Ui,
                );
                y += 24;
            }
            if !plan.held_back.is_empty() {
                ui_cards::draw_text_clipped(
                    text,
                    canvas,
                    &format!("{} 个软件包因版本锁定被保留", plan.held_back.len()),
                    list_clip.x,
                    y + 15,
                    list_clip.w,
                    11.5,
                    theme.warning,
                    Face::Ui,
                );
                y += 22;
            }
            ui_cards::draw_text_clipped(
                text,
                canvas,
                &format!("预计下载 {}", format_bytes(plan.total_download_bytes)),
                list_clip.x,
                y + 16,
                list_clip.w,
                12.0,
                theme.text_muted,
                Face::Ui,
            );
        }
        Err(error) => {
            for (idx, line) in ui_cards::wrap_text(text, error, list_clip.w as f32, 12.0, Face::Ui, 5)
                .into_iter()
                .enumerate()
            {
                ui_cards::draw_text_clipped(
                    text,
                    canvas,
                    &line,
                    list_clip.x,
                    y + 16 + idx as i32 * 20,
                    list_clip.w,
                    12.0,
                    theme.danger,
                    Face::Ui,
                );
            }
        }
    });

    if matches!(preview.job, TxJobKind::Remove { .. }) {
        let sw1 = Rect::new(card.x + 22, card.bottom() - 120, 36, 20);
        let row1 = Rect::new(card.x + 22, sw1.y - 4, 280, 28);
        ui::toggle_switch(
            canvas,
            theme,
            sw1,
            preview.recursive,
            prev_hits.hover_rect(pointer.0, pointer.1, row1),
        );
        ui_cards::draw_text(
            text,
            canvas,
            "同时移除无人依赖的依赖项",
            sw1.right() + 10,
            sw1.y + 15,
            12.0,
            theme.text,
            Face::Ui,
        );
        hits.push(row1, Action::ToggleRecursive);

        let sw2 = Rect::new(card.x + 22, card.bottom() - 88, 36, 20);
        let row2 = Rect::new(card.x + 22, sw2.y - 4, 280, 28);
        ui::toggle_switch(
            canvas,
            theme,
            sw2,
            preview.purge,
            prev_hits.hover_rect(pointer.0, pointer.1, row2),
        );
        ui_cards::draw_text(
            text,
            canvas,
            "同时删除配置文件",
            sw2.right() + 10,
            sw2.y + 15,
            12.0,
            theme.text,
            Face::Ui,
        );
        hits.push(row2, Action::TogglePurge);
    }

    let cancel = Rect::new(card.right() - 214, card.bottom() - 48, 92, 32);
    let hovered_c = prev_hits.hover_rect(pointer.0, pointer.1, cancel);
    ui_cards::cta_pill(canvas, text, theme, cancel, "取消", false, hovered_c, false, None);
    hits.push(cancel, Action::PreviewCancel);

    let confirm = Rect::new(card.right() - 112, card.bottom() - 48, 92, 32);
    let hovered_ok = prev_hits.hover_rect(pointer.0, pointer.1, confirm);
    ui_cards::cta_pill(canvas, text, theme, confirm, "执行", true, hovered_ok, false, None);
    hits.push(confirm, Action::PreviewConfirm);
}

#[allow(clippy::too_many_arguments)]
fn draw_auth(
    canvas: &mut Canvas,
    text: &mut TextRenderer,
    theme: &Theme,
    content: Rect,
    auth: &AuthPrompt,
    prev_hits: &Hits,
    hits: &mut Hits,
    pointer: (i32, i32),
) -> Option<Rect> {
    let card = Rect::new(
        content.x + (content.w - 472) / 2,
        content.y + (content.h - 284) / 2,
        472,
        284,
    );
    ui_cards::modal_frame(canvas, theme, content, card);
    hits.push(content, Action::Noop);

    let icon_plate = Rect::new(card.x + 20, card.y + 20, 36, 36);
    ui_cards::icon_badge(canvas, Icon::Shield, icon_plate, [theme.accent, ui_cards::mix(theme.accent, 0x000000, 0.28)]);
    ui_cards::modal_title(
        canvas,
        text,
        theme,
        Rect::new(card.x + 68, card.y + 10, card.w - 88, 60),
        "需要管理员授权",
        &format!("任务 #{} · {}", auth.job_id, auth.title),
    );
    ui_cards::draw_text_clipped(
        text,
        canvas,
        "将执行：",
        card.x + 22,
        card.y + 84,
        card.w - 44,
        11.5,
        theme.text_muted,
        Face::Ui,
    );
    let cmd_box = Rect::new(card.x + 22, card.y + 92, card.w - 44, 28);
    canvas.rounded_rect(cmd_box, 6, theme.panel, 1.0);
    canvas.rounded_border(cmd_box, 6, theme.panel_border, 0.85, 1);
    ui_cards::draw_text_clipped(
        text,
        canvas,
        &format!("sudo {}", auth.command),
        cmd_box.x + 10,
        cmd_box.y + 19,
        cmd_box.w - 20,
        11.5,
        theme.text,
        Face::Mono,
    );

    let input = Rect::new(card.x + 22, card.y + 132, card.w - 92, 34);
    let shown = if auth.reveal {
        auth.input.clone()
    } else {
        "•".repeat(auth.input.chars().count())
    };
    let caret = ui::text_input_ime(
        canvas,
        text,
        theme,
        input,
        &shown,
        "",
        "输入管理员密码",
        true,
        Some(Icon::Shield),
    );
    let reveal_btn = Rect::new(input.right() + 6, input.y, 44, 34);
    let rev_hov = prev_hits.hover_rect(pointer.0, pointer.1, reveal_btn);
    ui_cards::cta_pill(
        canvas,
        text,
        theme,
        reveal_btn,
        if auth.reveal { "隐藏" } else { "显示" },
        false,
        rev_hov,
        false,
        None,
    );
    hits.push(reveal_btn, Action::ToggleAuthReveal);

    if let Some(error) = &auth.error {
        for (idx, line) in ui_cards::wrap_text(text, error, (card.w - 44) as f32, 11.5, Face::Ui, 2)
            .into_iter()
            .enumerate()
        {
            ui_cards::draw_text_clipped(
                text,
                canvas,
                &line,
                card.x + 22,
                card.y + 186 + idx as i32 * 17,
                card.w - 44,
                11.5,
                theme.danger,
                Face::Ui,
            );
        }
    }

    let cancel = Rect::new(card.right() - 214, card.bottom() - 48, 92, 32);
    let hov_c = prev_hits.hover_rect(pointer.0, pointer.1, cancel);
    ui_cards::cta_pill(canvas, text, theme, cancel, "取消", false, hov_c, false, None);
    hits.push(cancel, Action::AuthCancel);

    let confirm = Rect::new(card.right() - 112, card.bottom() - 48, 92, 32);
    let hov_ok = prev_hits.hover_rect(pointer.0, pointer.1, confirm);
    ui_cards::cta_pill(canvas, text, theme, confirm, "授权", true, hov_ok, false, None);
    hits.push(confirm, Action::AuthSubmit);
    Some(caret)
}

fn draw_toast(
    canvas: &mut Canvas,
    text: &mut TextRenderer,
    theme: &Theme,
    content: Rect,
    message: &str,
    at: Instant,
    hits: &mut Hits,
) {
    let alpha = (1.0 - (at.elapsed().as_secs_f32() - 3.0).max(0.0)).clamp(0.0, 1.0);
    let width = (text.measure(message, 12.0, Face::Ui) as i32 + 40).min(content.w - 80);
    let rect = Rect::new(
        content.x + (content.w - width) / 2,
        content.y + content.h - 56,
        width.max(120),
        34,
    );
    canvas.rounded_drop_shadow(rect, 8, 12, 3, alpha * 0.25);
    canvas.rounded_rect(rect, 8, theme.window_high, alpha);
    canvas.rounded_border(rect, 8, theme.panel_border, alpha, 1);
    ui_cards::draw_text_clipped(
        text,
        canvas,
        message,
        rect.x + 16,
        rect.y + 22,
        rect.w - 32,
        12.0,
        theme.text,
        Face::Ui,
    );
    hits.push(rect, Action::DismissToast);
}
