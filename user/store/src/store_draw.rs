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
    canvas.clear(theme.bg_bottom);
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

    let brand_h = 58;
    icons::draw(
        canvas,
        Icon::Store,
        rect.x + 16,
        rect.y + 16,
        24,
        Some(theme.accent),
    );
    ui_cards::draw_text(
        text,
        canvas,
        "Space 商店",
        rect.x + 48,
        rect.y + 34,
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
    for (view, icon, label, count) in entries {
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
    let card = Rect::new(rect.x + 12, rect.y + rect.h - 78, rect.w - 24, 62);
    if !running.is_empty() {
        ui::card(canvas, theme, card);
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
            Rect::new(card.x + 10, card.y + 45, card.w - 20, 6),
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
    ui::toolbar_surface(canvas, theme, rect, false, true);
    let pointer = state.last_pointer;
    let mut x = rect.x + 18;

    if state.detail.is_some() {
        let back = Rect::new(x, rect.y + 11, 30, 30);
        let hovered = prev_hits.hover_rect(pointer.0, pointer.1, back);
        ui::icon_button(canvas, theme, back, Icon::GoBack, true, hovered);
        hits.push(back, Action::Back);
        x += 38;
    }

    let mut right = rect.right() - 18;
    let queue = Rect::new(right - 96, rect.y + 11, 96, 30);
    let hovered = prev_hits.hover_rect(pointer.0, pointer.1, queue);
    let label = if state.queue_open { "隐藏任务" } else { "任务队列" };
    ui_cards::cta_pill(canvas, text, theme, queue, label, false, hovered, false, None);
    hits.push(queue, Action::ToggleQueue);
    right -= 106;

    let sync = Rect::new(right - 104, rect.y + 11, 104, 30);
    let hovered = prev_hits.hover_rect(pointer.0, pointer.1, sync);
    ui_cards::cta_pill(
        canvas,
        text,
        theme,
        sync,
        "同步软件源",
        false,
        hovered,
        false,
        None,
    );
    hits.push(sync, Action::RefreshCatalog);
    right -= 114;

    let status = if state.offline {
        "离线模式".to_string()
    } else if state.loading {
        "正在读取软件源…".to_string()
    } else if let Some(repo) = state.repos.first() {
        format!(
            "{} · {} 个软件包 · {}",
            repo.name,
            repo.package_count,
            format_timestamp(repo.last_sync)
        )
    } else {
        String::new()
    };

    let min_title_w = 130;
    let max_status_w = (right - x - min_title_w - 16).max(0);
    if !status.is_empty() && max_status_w >= 88 {
        let width = (text.measure(&status, 11.0, Face::Bold) as i32 + 22).min(max_status_w);
        let pill = Rect::new(right - width, rect.y + 14, width, 24);
        ui_cards::badge(
            canvas,
            text,
            theme,
            pill,
            &status,
            if state.offline {
                theme.warning
            } else {
                theme.success
            },
        );
        right = pill.x - 14;
    }

    let title = match (state.detail.as_deref(), state.view) {
        (Some(name), _) => state
            .package(name)
            .map(|pkg| state.presentation(pkg).display_name)
            .unwrap_or_else(|| name.to_string()),
        (None, NavSection::Discover) => "探索精选".to_string(),
        (None, NavSection::Browse) => "分类浏览".to_string(),
        (None, NavSection::Updates) => "软件更新".to_string(),
        (None, NavSection::Installed) => "已安装".to_string(),
        (None, NavSection::Maintenance) => "仓库与维护".to_string(),
    };
    ui_cards::draw_text_clipped(
        text,
        canvas,
        &title,
        x,
        rect.y + 32,
        (right - x).max(60),
        16.0,
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
    let sections = build_sections(&state.catalog);
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
                let banner = Rect::new(area.x, y, inner_w, 58);
                if banner.bottom() >= area.y && banner.y <= area.bottom() {
                    ui::card(canvas, theme, banner);
                    ui_cards::draw_text_clipped(
                        text,
                        canvas,
                        &format!("{} 个软件包有新版本", state_ref.updates.len()),
                        banner.x + 16,
                        banner.y + 24,
                        banner.w - 150,
                        13.5,
                        theme.text,
                        Face::Bold,
                    );
                    ui_cards::draw_text_clipped(
                        text,
                        canvas,
                        "查看更新中心可逐项确认版本变化与回滚保障",
                        banner.x + 16,
                        banner.y + 42,
                        banner.w - 150,
                        11.5,
                        theme.text_muted,
                        Face::Ui,
                    );
                    let cta = Rect::new(banner.right() - 122, banner.y + 15, 106, 28);
                    let hovered = prev_hits.hover_rect(pointer.0, pointer.1, cta);
                    ui_cards::cta_pill(
                        canvas, text, theme, cta, "查看更新", true, hovered, false, None,
                    );
                    hits.push_clipped(cta, area, Action::Nav(NavSection::Updates));
                }
                y += 74;
            }

            let columns = if inner_w >= 820 {
                3
            } else if inner_w >= 520 {
                2
            } else {
                1
            };
            let card_w = (inner_w - (columns - 1) * 16) / columns;

            for section in &sections {
                let header = Rect::new(area.x, y, inner_w, 42);
                if header.bottom() >= area.y && header.y <= area.bottom() {
                    ui_cards::section_header(
                        canvas,
                        text,
                        theme,
                        header,
                        &section.title,
                        &section.subtitle,
                    );
                }
                y += 46;

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

    let mut x = area.x;
    for category in StoreCategory::ALL {
        let label = category_label(&state.catalog, category);
        let width = text.measure(&label, 12.0, Face::Ui) as i32 + 28;
        let pill = Rect::new(x, y, width, 28);
        let selected = state.category == category;
        let hovered = prev_hits.hover_rect(pointer.0, pointer.1, pill);
        let color = ui::filter_pill(canvas, theme, pill, selected, hovered);
        let tw = text.measure(&label, 12.0, Face::Ui);
        text.draw(
            canvas,
            &label,
            (pill.x + pill.w / 2) as f32 - tw / 2.0,
            (pill.y + 18) as f32,
            12.0,
            color,
            if selected { Face::Bold } else { Face::Ui },
        );
        hits.push(pill, Action::Category(category));
        x += width + 8;
    }
    y += 40;

    let mut x = area.x;
    ui_cards::draw_text(
        text,
        canvas,
        "排序",
        x,
        y + 18,
        11.5,
        theme.text_muted,
        Face::Ui,
    );
    x += 34;
    for sort in SortMode::ALL {
        let width = text.measure(sort.label(), 11.5, Face::Ui) as i32 + 24;
        let pill = Rect::new(x, y, width, 26);
        let selected = state.sort == sort;
        let hovered = prev_hits.hover_rect(pointer.0, pointer.1, pill);
        let color = ui::filter_pill(canvas, theme, pill, selected, hovered);
        let tw = text.measure(sort.label(), 11.5, Face::Ui);
        text.draw(
            canvas,
            sort.label(),
            (pill.x + pill.w / 2) as f32 - tw / 2.0,
            (pill.y + 17) as f32,
            11.5,
            color,
            if selected { Face::Bold } else { Face::Ui },
        );
        hits.push(pill, Action::Sort(sort));
        x += width + 6;
    }
    y += 38;

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
            "试试更换分类、清空搜索词，或点右上角「同步软件源」刷新索引。",
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
                let cta_approx = Rect::new(card.right() - 90, card.bottom() - 38, 76, 26);
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
    let mut y = area.y;
    let count = state.updates.len();
    ui_cards::draw_text_clipped(
        text,
        canvas,
        &if count == 0 {
            "系统已是最新".to_string()
        } else {
            format!("{count} 个软件包可以更新")
        },
        area.x,
        y + 18,
        area.w - 260,
        15.0,
        theme.text,
        Face::Bold,
    );
    let all = Rect::new(area.right() - 118, y, 118, 28);
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
    let check = Rect::new(area.right() - 236, y, 110, 28);
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
    y += 44;

    let viewport = Rect::new(area.x, y, area.w, (area.bottom() - y).max(40));
    if count == 0 {
        ui_cards::empty_state(
            canvas,
            text,
            theme,
            viewport,
            Icon::Refresh,
            "没有待处理的更新",
            "同步软件源后可以再次检查新版本。",
        );
        return;
    }

    let inner_w = (viewport.w - SCROLLBAR_GUTTER).max(160);
    let row_h = 60;
    let total_h = (count as i32 * (row_h + 10)).max(viewport.h);
    let sb_rect = Rect::new(viewport.right() - 8, viewport.y, 8, viewport.h);
    state
        .grid_scroll
        .set_bounds(viewport.h as f32, total_h as f32, sb_rect);
    let offset = state.grid_scroll.offset_i32();
    let update_names = state.updates.clone();

    {
        let state_ref: &StoreState = state;
        canvas.with_clip(viewport, |canvas| {
            for (index, name) in update_names.iter().enumerate() {
                let Some(pkg) = state_ref.package(name) else {
                    continue;
                };
                let row = Rect::new(
                    viewport.x,
                    viewport.y + index as i32 * (row_h + 10) - offset,
                    inner_w,
                    row_h,
                );
                if row.bottom() < viewport.y || row.y > viewport.bottom() {
                    continue;
                }
                ui::card(canvas, theme, row);
                let pres = state_ref.presentation(pkg);
                ui_cards::icon_badge(
                    canvas,
                    pres.icon,
                    Rect::new(row.x + 14, row.y + 14, 32, 32),
                    pres.accent,
                );
                ui_cards::draw_text_clipped(
                    text,
                    canvas,
                    &pres.display_name,
                    row.x + 58,
                    row.y + 25,
                    row.w - 280,
                    13.5,
                    theme.text,
                    Face::Bold,
                );
                let from = pkg.installed_version.as_deref().unwrap_or("-");
                let to = pkg.available_version.as_deref().unwrap_or("-");
                ui_cards::draw_text_clipped(
                    text,
                    canvas,
                    &format!("{from} → {to} · {}", format_bytes(pkg.size)),
                    row.x + 58,
                    row.y + 44,
                    row.w - 280,
                    11.5,
                    theme.text_muted,
                    Face::Ui,
                );
                if pkg.held {
                    let pill = Rect::new(row.right() - 196, row.y + 19, 60, 22);
                    ui_cards::badge(canvas, text, theme, pill, "已锁定", theme.warning);
                }
                hits.push_clipped(row, viewport, Action::OpenDetail(pkg.name.clone()));

                let cta = Rect::new(row.right() - 122, row.y + 16, 102, 28);
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
                        canvas, text, theme, cta, "解除锁定", false, hovered, false, None,
                    );
                    hits.push_clipped(cta, viewport, Action::Unhold(pkg.name.clone()));
                } else {
                    ui_cards::cta_pill(
                        canvas, text, theme, cta, "更新", true, hovered, false, None,
                    );
                    hits.push_clipped(cta, viewport, Action::Update(pkg.name.clone()));
                }
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
    let mut y = area.y;
    let mut x = area.x;
    let counts = [
        state.installed_count(),
        state.explicit_count(),
        state.dependency_count(),
        state.held_count(),
    ];
    for (index, filter) in InstalledFilter::ALL.iter().enumerate() {
        let label = format!("{} {}", filter.label(), counts[index]);
        let width = text.measure(&label, 12.0, Face::Ui) as i32 + 28;
        let pill = Rect::new(x, y, width, 28);
        let selected = state.installed_filter == *filter;
        let hovered = prev_hits.hover_rect(pointer.0, pointer.1, pill);
        let color = ui::filter_pill(canvas, theme, pill, selected, hovered);
        let tw = text.measure(&label, 12.0, Face::Ui);
        text.draw(
            canvas,
            &label,
            (pill.x + pill.w / 2) as f32 - tw / 2.0,
            (pill.y + 18) as f32,
            12.0,
            color,
            if selected { Face::Bold } else { Face::Ui },
        );
        hits.push(pill, Action::InstalledFilter(*filter));
        x += width + 8;
    }
    y += 42;

    let viewport = Rect::new(area.x, y, area.w, (area.bottom() - y).max(40));
    let packages: Vec<StorePackage> = state.filtered_packages().into_iter().cloned().collect();
    if packages.is_empty() {
        ui_cards::empty_state(
            canvas,
            text,
            theme,
            viewport,
            Icon::Folder,
            "这个筛选下没有软件包",
            "切换筛选条件，或在「分类浏览」里安装新软件。",
        );
        return;
    }

    let inner_w = (viewport.w - SCROLLBAR_GUTTER).max(160);
    let row_h = 56;
    let total_h = (packages.len() as i32 * (row_h + 8)).max(viewport.h);
    let sb_rect = Rect::new(viewport.right() - 8, viewport.y, 8, viewport.h);
    state
        .grid_scroll
        .set_bounds(viewport.h as f32, total_h as f32, sb_rect);
    let offset = state.grid_scroll.offset_i32();

    {
        let state_ref: &StoreState = state;
        canvas.with_clip(viewport, |canvas| {
            for (index, pkg) in packages.iter().enumerate() {
                let row = Rect::new(
                    viewport.x,
                    viewport.y + index as i32 * (row_h + 8) - offset,
                    inner_w,
                    row_h,
                );
                if row.bottom() < viewport.y || row.y > viewport.bottom() {
                    continue;
                }
                let hovered = prev_hits.hover_rect(pointer.0, pointer.1, row);
                ui::list_item_bg(canvas, theme, row, false, hovered);
                canvas.rounded_border(row, theme.radius_control(), theme.panel_border, 0.5, 1);

                let pres = state_ref.presentation(pkg);
                ui_cards::icon_badge(
                    canvas,
                    pres.icon,
                    Rect::new(row.x + 12, row.y + 12, 32, 32),
                    pres.accent,
                );
                ui_cards::draw_text_clipped(
                    text,
                    canvas,
                    &pres.display_name,
                    row.x + 56,
                    row.y + 24,
                    row.w - 370,
                    13.5,
                    theme.text,
                    Face::Bold,
                );
                let detail = match (pkg.installed_version.as_deref(), pkg.installed_size) {
                    (Some(version), Some(size)) => format!("{version} · {}", format_bytes(size)),
                    (Some(version), None) => version.to_string(),
                    _ => String::new(),
                };
                ui_cards::draw_text_clipped(
                    text,
                    canvas,
                    &detail,
                    row.x + 56,
                    row.y + 42,
                    row.w - 370,
                    11.0,
                    theme.text_muted,
                    Face::Ui,
                );
                if pkg.held {
                    let pill = Rect::new(row.right() - 304, row.y + 17, 56, 22);
                    ui_cards::badge(canvas, text, theme, pill, "已锁定", theme.warning);
                }

                // Register row BEFORE action buttons so button hits always win cleanly.
                hits.push_clipped(row, viewport, Action::OpenDetail(pkg.name.clone()));

                let hold = Rect::new(row.right() - 236, row.y + 14, 66, 28);
                let hov_h = prev_hits.hover_rect(pointer.0, pointer.1, hold);
                ui_cards::cta_pill(
                    canvas,
                    text,
                    theme,
                    hold,
                    if pkg.held { "解锁" } else { "锁定" },
                    false,
                    hov_h,
                    false,
                    None,
                );
                hits.push_clipped(
                    hold,
                    viewport,
                    if pkg.held {
                        Action::Unhold(pkg.name.clone())
                    } else {
                        Action::Hold(pkg.name.clone())
                    },
                );

                let reinstall = Rect::new(row.right() - 162, row.y + 14, 66, 28);
                let hov_r = prev_hits.hover_rect(pointer.0, pointer.1, reinstall);
                ui_cards::cta_pill(
                    canvas, text, theme, reinstall, "重装", false, hov_r, false, None,
                );
                hits.push_clipped(reinstall, viewport, Action::Reinstall(pkg.name.clone()));

                let remove = Rect::new(row.right() - 88, row.y + 14, 68, 28);
                let hov_d = prev_hits.hover_rect(pointer.0, pointer.1, remove);
                ui_cards::cta_pill(
                    canvas, text, theme, remove, "卸载", false, hov_d, false, None,
                );
                hits.push_clipped(remove, viewport, Action::Remove(pkg.name.clone()));
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
        canvas.fill(main, theme.bg_bottom);
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
    canvas.fill(main, theme.bg_bottom);
    let pres = state.presentation(&pkg);
    let area = content_scroll_area(main);
    let header_h = 164;
    let header = Rect::new(area.x, area.y, area.w, header_h);
    ui::card(canvas, theme, header);

    let show_preview = (pres.banner.is_some() || pres.is_theme) && header.w >= 720;
    let preview_w = if show_preview { 244 } else { 0 };
    if show_preview {
        let pr = Rect::new(header.right() - preview_w - 16, header.y + 16, preview_w, 132);
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
        Rect::new(header.x + 20, header.y + 20, 76, 76),
        pres.accent,
    );
    let info_x = header.x + 112;
    let info_w = (header.w - 128 - if show_preview { preview_w + 24 } else { 0 }).max(120);
    ui_cards::draw_text_clipped(
        text,
        canvas,
        &pres.display_name,
        info_x,
        header.y + 40,
        info_w,
        20.0,
        theme.text,
        Face::Bold,
    );
    ui_cards::draw_text_clipped(
        text,
        canvas,
        &pres.subtitle,
        info_x,
        header.y + 64,
        info_w,
        12.5,
        theme.text_muted,
        Face::Ui,
    );
    ui_cards::draw_text_clipped(
        text,
        canvas,
        &format!(
            "{} · {} · {}",
            pkg.display_version(),
            pkg.arch,
            format_bytes(pkg.size)
        ),
        info_x,
        header.y + 84,
        info_w,
        11.5,
        theme.text_muted,
        Face::Ui,
    );

    let pointer = state.last_pointer;
    let mut button_x = info_x;
    let btn_y = header.y + 108;
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
        ui_cards::cta_pill(canvas, text, theme, remove, "卸载", false, hovered, false, None);
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

    let stats = Rect::new(area.x, header.bottom() + 12, area.w, 64);
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

    let tabs = Rect::new(area.x, stats.bottom() + 12, area.w, 32);
    let mut tab_x = tabs.x;
    for (tab, label) in [
        (crate::model::DetailTab::Overview, "概览"),
        (crate::model::DetailTab::Files, "文件列表"),
    ] {
        let width = text.measure(label, 12.5, Face::Ui) as i32 + 32;
        let rect = Rect::new(tab_x, tabs.y, width, 30);
        let active = state.detail_tab == tab;
        let hovered = prev_hits.hover_rect(pointer.0, pointer.1, rect);
        let color = ui::tab_item(canvas, theme, rect, active, hovered, false, None);
        let tw = text.measure(label, 12.5, Face::Ui);
        text.draw(
            canvas,
            label,
            (rect.x + rect.w / 2) as f32 - tw / 2.0,
            (rect.y + 20) as f32,
            12.5,
            color,
            if active { Face::Bold } else { Face::Ui },
        );
        hits.push(rect, Action::SetTab(tab));
        tab_x += width + 4;
    }

    let body = Rect::new(
        area.x,
        tabs.bottom() + 10,
        area.w,
        (area.bottom() - tabs.bottom() - 10).max(40),
    );
    let inner_w = (body.w - SCROLLBAR_GUTTER).max(160);
    let offset = state.detail_scroll.offset_i32();
    let total_h: i32;

    {
        let state_ref: &StoreState = state;
        let mut y = body.y - offset;
        canvas.with_clip(body, |canvas| match state_ref.detail_tab {
            crate::model::DetailTab::Overview => {
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

    let sb_rect = Rect::new(body.right() - 8, body.y, 8, body.h);
    state
        .detail_scroll
        .set_bounds(body.h as f32, total_h as f32, sb_rect);
    ui::scrollbar_v_animated(
        canvas,
        theme,
        "store_detail_scrollbar",
        sb_rect,
        total_h as f32,
        body.h as f32,
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
                Rect::new(viewport.x, y, inner_w, 40),
                "软件源",
                "优先级越高越先被采用；至少保留一个启用的软件源。",
            );
            y += 46;

            for repo in &state_ref.repos {
                let row = Rect::new(viewport.x, y, inner_w, 56);
                ui::card(canvas, theme, row);
                ui_cards::draw_text_clipped(
                    text,
                    canvas,
                    &repo.name,
                    row.x + 14,
                    row.y + 22,
                    row.w - 220,
                    13.0,
                    theme.text,
                    Face::Bold,
                );
                ui_cards::draw_text_clipped(
                    text,
                    canvas,
                    &format!(
                        "优先级 {} · 架构 {} · {} 个软件包 · {}",
                        repo.priority,
                        repo.arch,
                        repo.package_count,
                        format_timestamp(repo.last_sync)
                    ),
                    row.x + 14,
                    row.y + 42,
                    row.w - 220,
                    11.0,
                    theme.text_muted,
                    Face::Ui,
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

            let form = Rect::new(viewport.x, y, inner_w, 82);
            ui::card(canvas, theme, form);
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
            ui_cards::draw_text_clipped(
                text,
                canvas,
                "点击输入框即可键入名称与地址（Tab 切换焦点，Enter 提交）。",
                form.x + 14,
                form.y + 66,
                form.w - 28,
                11.0,
                theme.text_muted,
                Face::Ui,
            );
            y += 96;

            ui_cards::section_header(
                canvas,
                text,
                theme,
                Rect::new(viewport.x, y, inner_w, 40),
                "信任密钥",
                "Ed25519 公钥，用于校验仓库索引与 store.json 签名。",
            );
            y += 46;
            for key in &state_ref.trust_keys {
                let row = Rect::new(viewport.x, y, inner_w, 42);
                ui::list_item_bg(canvas, theme, row, false, false);
                canvas.rounded_border(row, theme.radius_control(), theme.panel_border, 0.45, 1);
                icons::draw(canvas, Icon::Shield, row.x + 12, row.y + 13, 16, Some(theme.success));
                ui_cards::draw_text_clipped(
                    text,
                    canvas,
                    &key.key_id,
                    row.x + 36,
                    row.y + 25,
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
                    row.y + 25,
                    168,
                    11.0,
                    theme.text_muted,
                    Face::Ui,
                );
                y += 48;
            }
            let trust_row = Rect::new(viewport.x, y, inner_w, 48);
            ui::card(canvas, theme, trust_row);
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
                Rect::new(viewport.x, y, inner_w, 40),
                "缓存与一致性",
                "缓存只保存已下载的软件包，可随时清理并重新校验。",
            );
            y += 46;
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
                ui::card(canvas, theme, panel);
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
