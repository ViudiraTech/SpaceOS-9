// Drawing layer for Space 商店, included from store.rs so it can share the
// window state without exposing it.

fn draw_store(
    canvas: &mut Canvas,
    text: &mut TextRenderer,
    theme: &Theme,
    state: &mut StoreState,
    content: Rect,
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

    draw_sidebar(canvas, text, theme, state, sidebar, hits);
    draw_topbar(canvas, text, theme, state, topbar, hits);

    match state.view {
        NavSection::Discover => draw_discover(canvas, text, theme, state, main, hits),
        NavSection::Browse => draw_browse(canvas, text, theme, state, main, hits),
        NavSection::Updates => draw_updates(canvas, text, theme, state, main, hits),
        NavSection::Installed => draw_installed(canvas, text, theme, state, main, hits),
        NavSection::Maintenance => draw_maintenance(canvas, text, theme, state, main, hits),
    }

    if state.detail.is_some() {
        draw_detail(canvas, text, theme, state, main, hits);
    }

    if state.queue_open {
        let queue = Rect::new(content.x, content.y + content.h - QUEUE_H, content.w, QUEUE_H);
        draw_queue(canvas, text, theme, state, queue, hits);
    }

    let mut ime = None;
    if let Some(preview) = state.preview.as_ref() {
        draw_preview(
            canvas,
            text,
            theme,
            content,
            preview,
            hits,
            state.last_pointer,
        );
    }
    if let Some(auth) = state.auth.as_ref() {
        ime = draw_auth(canvas, text, theme, content, auth, hits);
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
    hits: &mut Hits,
) {
    ui::sidebar_surface(canvas, theme, rect);
    let pointer = state.last_pointer;

    let brand_h = 56;
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
        rect.y + 33,
        15.0,
        theme.text,
        Face::Bold,
    );

    let search = Rect::new(rect.x + 12, rect.y + brand_h, rect.w - 24, 30);
    ui::text_input_ime(
        canvas,
        text,
        theme,
        search,
        &state.search,
        &state.preedit,
        "搜索软件包",
        state.search_focus,
        Some(Icon::Files),
    );
    hits.push(search, Action::FocusSearch);

    let entries: [(NavSection, Icon, &str, usize); 5] = [
        (NavSection::Discover, Icon::Store, "探索精选", 0),
        (NavSection::Browse, Icon::Applications, "分类浏览", 0),
        (NavSection::Updates, Icon::Refresh, "软件更新", state.updates.len()),
        (NavSection::Installed, Icon::Folder, "已安装", state.installed_count()),
        (NavSection::Maintenance, Icon::Settings, "仓库与维护", 0),
    ];
    let mut y = rect.y + brand_h + 40;
    for (view, icon, label, count) in entries {
        let item = Rect::new(rect.x + 8, y, rect.w - 16, 34);
        let active = state.view == view && state.detail.is_none();
        let hovered = hits.hover_rect(pointer.0, pointer.1, item);
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
        y += 38;
    }

    // running jobs mini card at the bottom
    let running: Vec<&TxJob> = state.jobs.iter().filter(|j| j.status.is_active()).collect();
    let card = Rect::new(rect.x + 10, rect.y + rect.h - 76, rect.w - 20, 60);
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
            Rect::new(card.x + 10, card.y + 44, card.w - 20, 6),
            job.percent as f32 / 100.0,
            Some(theme.accent),
        );
        hits.push(card, Action::ToggleQueue);
    }
}

fn draw_topbar(
    canvas: &mut Canvas,
    text: &mut TextRenderer,
    theme: &Theme,
    state: &StoreState,
    rect: Rect,
    hits: &mut Hits,
) {
    ui::toolbar_surface(canvas, theme, rect, false, true);
    let pointer = state.last_pointer;
    let mut x = rect.x + 16;

    if state.detail.is_some() {
        let back = Rect::new(x, rect.y + 10, 28, 28);
        let hovered = hits.hover_rect(pointer.0, pointer.1, back);
        ui::icon_button(canvas, theme, back, Icon::GoBack, true, hovered);
        hits.push(back, Action::Back);
        x += 36;
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
        rect.y + 30,
        rect.w - 420,
        15.0,
        theme.text,
        Face::Bold,
    );

    let mut right = rect.x + rect.w - 16;
    let queue = Rect::new(right - 96, rect.y + 10, 96, 28);
    let hovered = hits.hover_rect(pointer.0, pointer.1, queue);
    let label = if state.queue_open { "隐藏任务" } else { "任务队列" };
    ui_cards::cta_pill(canvas, text, theme, queue, label, false, hovered, false, None);
    hits.push(queue, Action::ToggleQueue);
    right -= 104;

    let sync = Rect::new(right - 104, rect.y + 10, 104, 28);
    let hovered = hits.hover_rect(pointer.0, pointer.1, sync);
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
    right -= 112;

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
    if !status.is_empty() {
        let width = text.measure(&status, 11.0, Face::Ui) as i32 + 20;
        let pill = Rect::new(right - width, rect.y + 12, width, 24);
        ui_cards::badge(canvas, theme, pill, &status, theme.success);
        ui_cards::draw_text_clipped(
            text,
            canvas,
            &status,
            pill.x + 10,
            pill.y + 16,
            pill.w - 20,
            11.0,
            theme.text_muted,
            Face::Ui,
        );
    }
}

fn content_scroll_area(state: &StoreState, main: Rect) -> Rect {
    let _ = state;
    Rect::new(main.x + 20, main.y + 12, main.w - 40, main.h - 24)
}

fn draw_discover(
    canvas: &mut Canvas,
    text: &mut TextRenderer,
    theme: &Theme,
    state: &mut StoreState,
    main: Rect,
    hits: &mut Hits,
) {
    let area = content_scroll_area(state, main);
    let slides = state.hero_slides();
    let mut y = area.y;

    if !slides.is_empty() {
        let hero = Rect::new(area.x, y, area.w, HERO_H);
        let index = state.hero_index.min(slides.len() - 1);
        let slide = slides[index].clone();
        let installed = slide
            .target_package
            .as_deref()
            .and_then(|name| state.package(name))
            .is_some_and(|pkg| pkg.installed);
        let hovered = hero.contains(state.last_pointer.0, state.last_pointer.1);
        let cta = ui_cards::hero_card(canvas, text, theme, hero, &slide, installed, hovered);
        hits.push(cta, Action::HeroCta(index));

        let dots_y = hero.y + hero.h - 18;
        for dot in 0..slides.len() {
            let rect = Rect::new(hero.x + 24 + dot as i32 * 18, dots_y, 8, 8);
            let color = if dot == index {
                0xffffff
            } else {
                ui_cards::mix(0xffffff, slide.grad_right, 0.55)
            };
            canvas.rounded_rect(rect, 4, color, if dot == index { 1.0 } else { 0.5 });
            hits.push(Rect::new(rect.x - 4, rect.y - 6, 16, 20), Action::HeroDot(dot));
        }
        y += HERO_H + 16;
    }

    if !state.updates.is_empty() {
        let banner = Rect::new(area.x, y, area.w, 54);
        ui::card(canvas, theme, banner);
        ui_cards::draw_text_clipped(
            text,
            canvas,
            &format!("{} 个软件包有新版本", state.updates.len()),
            banner.x + 14,
            banner.y + 22,
            banner.w - 220,
            13.5,
            theme.text,
            Face::Bold,
        );
        ui_cards::draw_text_clipped(
            text,
            canvas,
            "查看更新中心可逐项确认版本变化",
            banner.x + 14,
            banner.y + 40,
            banner.w - 220,
            11.5,
            theme.text_muted,
            Face::Ui,
        );
        let cta = Rect::new(banner.x + banner.w - 118, banner.y + 13, 104, 28);
        let hovered = hits.hover_rect(state.last_pointer.0, state.last_pointer.1, cta);
        ui_cards::cta_pill(canvas, text, theme, cta, "查看更新", true, hovered, false, None);
        hits.push(cta, Action::Nav(NavSection::Updates));
        y += 70;
    }

    for section in build_sections(&state.catalog) {
        let header = Rect::new(area.x, y, area.w, 40);
        ui_cards::section_header(canvas, text, theme, header, &section.title, &section.subtitle);
        y += 44;

        let columns = ((area.w + 16) / 300).max(1);
        let card_w = (area.w - (columns - 1) * 16) / columns;
        let mut row_y = y;
        for (index, name) in section.packages.iter().enumerate() {
            if index as i32 % columns == 0 && index > 0 {
                row_y += ui_cards::CARD_H + 14;
            }
            let col = index as i32 % columns;
            let card = Rect::new(
                area.x + col * (card_w + 16),
                row_y,
                card_w,
                ui_cards::CARD_H,
            );
            if let Some(pkg) = state.package(name).cloned() {
                let pres = state.presentation(&pkg);
                let hovered = hits.hover_rect(state.last_pointer.0, state.last_pointer.1, card);
                let progress = state
                    .job_for(&pkg.name)
                    .map(|job| job.percent as f32 / 100.0);
                let cta = ui_cards::package_card(
                    canvas, text, theme, card, &pkg, &pres, false, hovered, progress,
                );
                hits.push(card, Action::OpenDetail(pkg.name.clone()));
                if let Some(job) = state.job_for(&pkg.name) {
                    let _ = job;
                    hits.push(cta, Action::ToggleQueue);
                } else if pkg.upgrade_available {
                    hits.push(cta, Action::Update(pkg.name.clone()));
                } else if pkg.installed {
                    hits.push(cta, Action::OpenDetail(pkg.name.clone()));
                } else {
                    hits.push(cta, Action::Install(pkg.name.clone()));
                }
            }
        }
        let rows = (section.packages.len() as i32 + columns - 1) / columns;
        y = row_y + rows * (ui_cards::CARD_H + 14) + 8;
    }
}

fn draw_browse(
    canvas: &mut Canvas,
    text: &mut TextRenderer,
    theme: &Theme,
    state: &mut StoreState,
    main: Rect,
    hits: &mut Hits,
) {
    let area = content_scroll_area(state, main);
    let mut y = area.y;

    // category pills
    let mut x = area.x;
    for category in StoreCategory::ALL {
        let label = category_label(&state.catalog, category);
        let width = text.measure(&label, 12.0, Face::Ui) as i32 + 28;
        let pill = Rect::new(x, y, width, 28);
        let selected = state.category == category;
        let hovered = hits.hover_rect(state.last_pointer.0, state.last_pointer.1, pill);
        let color = ui::filter_pill(canvas, theme, pill, selected, hovered);
        let tw = text.measure(&label, 12.0, Face::Ui);
        text.draw(
            canvas,
            &label,
            (pill.x + pill.w / 2) as f32 - tw / 2.0,
            (pill.y + 18) as f32,
            12.0,
            color,
            Face::Ui,
        );
        hits.push(pill, Action::Category(category));
        x += width + 8;
    }
    y += 40;

    // sort pills
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
        let hovered = hits.hover_rect(state.last_pointer.0, state.last_pointer.1, pill);
        let color = ui::filter_pill(canvas, theme, pill, selected, hovered);
        let tw = text.measure(sort.label(), 11.5, Face::Ui);
        text.draw(
            canvas,
            sort.label(),
            (pill.x + pill.w / 2) as f32 - tw / 2.0,
            (pill.y + 17) as f32,
            11.5,
            color,
            Face::Ui,
        );
        hits.push(pill, Action::Sort(sort));
        x += width + 6;
    }
    y += 38;

    let packages: Vec<StorePackage> = state
        .filtered_packages()
        .into_iter()
        .cloned()
        .collect();

    if packages.is_empty() {
        ui_cards::empty_state(
            canvas,
            text,
            theme,
            Rect::new(area.x, y, area.w, area.h - (y - area.y)),
            Icon::Files,
            if state.search.is_empty() {
                "没有可显示的软件包"
            } else {
                "没有匹配的软件包"
            },
            "试试更换分类，或点右上角「同步软件源」刷新索引。",
        );
        return;
    }

    let columns = ((area.w + 16) / 300).max(1);
    let card_w = (area.w - (columns - 1) * 16) / columns;
    let rows = (packages.len() as i32 + columns - 1) / columns;
    let total_h = rows * (ui_cards::CARD_H + 14);
    let viewport = Rect::new(area.x, y, area.w, area.h - (y - area.y));
    state.grid_scroll.set_bounds(
        viewport.h as f32,
        total_h as f32,
        Rect::new(area.x + area.w - 8, viewport.y, 8, viewport.h),
    );
    let offset = state.grid_scroll.offset_i32();
    for (index, pkg) in packages.iter().enumerate() {
        let col = index as i32 % columns;
        let row = index as i32 / columns;
        let card = Rect::new(
            area.x + col * (card_w + 16),
            y + row * (ui_cards::CARD_H + 14) - offset,
            card_w,
            ui_cards::CARD_H,
        );
        if card.y + card.h < viewport.y || card.y > viewport.y + viewport.h {
            continue;
        }
        let pres = state.presentation(pkg);
        let hovered = hits.hover_rect(state.last_pointer.0, state.last_pointer.1, card);
        let progress = state.job_for(&pkg.name).map(|job| job.percent as f32 / 100.0);
        let cta = ui_cards::package_card(
            canvas, text, theme, card, pkg, &pres, false, hovered, progress,
        );
        hits.push(card, Action::OpenDetail(pkg.name.clone()));
        if state.job_for(&pkg.name).is_some() {
            hits.push(cta, Action::ToggleQueue);
        } else if pkg.upgrade_available {
            hits.push(cta, Action::Update(pkg.name.clone()));
        } else if pkg.installed {
            hits.push(cta, Action::OpenDetail(pkg.name.clone()));
        } else {
            hits.push(cta, Action::Install(pkg.name.clone()));
        }
    }
    ui::scrollbar_v_animated(
        canvas,
        theme,
        "store_grid_scrollbar",
        Rect::new(area.x + area.w - 8, viewport.y, 8, viewport.h),
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
    hits: &mut Hits,
) {
    let area = content_scroll_area(state, main);
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
        y + 16,
        area.w - 260,
        15.0,
        theme.text,
        Face::Bold,
    );
    let all = Rect::new(area.x + area.w - 118, y, 118, 28);
    let hovered = hits.hover_rect(state.last_pointer.0, state.last_pointer.1, all);
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
    let check = Rect::new(area.x + area.w - 236, y, 110, 28);
    let hovered = hits.hover_rect(state.last_pointer.0, state.last_pointer.1, check);
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

    if count == 0 {
        ui_cards::empty_state(
            canvas,
            text,
            theme,
            Rect::new(area.x, y, area.w, area.h - (y - area.y)),
            Icon::Refresh,
            "没有待处理的更新",
            "同步软件源后可以再次检查新版本。",
        );
        return;
    }

    let viewport = Rect::new(area.x, y, area.w, area.h - (y - area.y));
    let row_h = 56;
    let total_h = count as i32 * (row_h + 8);
    state.grid_scroll.set_bounds(
        viewport.h as f32,
        total_h as f32,
        Rect::new(area.x + area.w - 8, viewport.y, 8, viewport.h),
    );
    let offset = state.grid_scroll.offset_i32();
    for (index, name) in state.updates.clone().iter().enumerate() {
        let Some(pkg) = state.package(name).cloned() else {
            continue;
        };
        let row = Rect::new(
            area.x,
            y + index as i32 * (row_h + 8) - offset,
            area.w,
            row_h,
        );
        if row.y + row.h < viewport.y || row.y > viewport.y + viewport.h {
            continue;
        }
        ui::card(canvas, theme, row);
        let pres = state.presentation(&pkg);
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
            row.w - 320,
            13.0,
            theme.text,
            Face::Bold,
        );
        let from = pkg.installed_version.clone().unwrap_or_else(|| "-".into());
        let to = pkg.available_version.clone().unwrap_or_else(|| "-".into());
        ui_cards::draw_text_clipped(
            text,
            canvas,
            &format!("{from} → {to} · {}", format_bytes(pkg.size)),
            row.x + 56,
            row.y + 42,
            row.w - 320,
            11.5,
            theme.text_muted,
            Face::Ui,
        );
        if pkg.held {
            let pill = Rect::new(row.x + row.w - 200, row.y + 16, 60, 22);
            ui_cards::badge(canvas, theme, pill, "已锁定", theme.warning);
            ui_cards::draw_text(
                text,
                canvas,
                "已锁定",
                pill.x + 10,
                pill.y + 15,
                11.0,
                theme.warning,
                Face::Ui,
            );
        }
        let cta = Rect::new(row.x + row.w - 124, row.y + 14, 96, 28);
        let hovered = hits.hover_rect(state.last_pointer.0, state.last_pointer.1, cta);
        if let Some(job) = state.job_for(&pkg.name) {
            ui_cards::inline_progress(canvas, theme, cta, job.percent as f32 / 100.0, theme.accent);
            hits.push(cta, Action::ToggleQueue);
        } else if pkg.held {
            ui_cards::cta_pill(canvas, text, theme, cta, "解除锁定", false, hovered, false, None);
            hits.push(cta, Action::Unhold(pkg.name.clone()));
        } else {
            ui_cards::cta_pill(canvas, text, theme, cta, "更新", true, hovered, false, None);
            hits.push(cta, Action::Update(pkg.name.clone()));
        }
        hits.push(row, Action::OpenDetail(pkg.name.clone()));
    }
}

fn draw_installed(
    canvas: &mut Canvas,
    text: &mut TextRenderer,
    theme: &Theme,
    state: &mut StoreState,
    main: Rect,
    hits: &mut Hits,
) {
    let area = content_scroll_area(state, main);
    let mut y = area.y;
    let mut x = area.x;
    let counts = [
        state.installed_count(),
        state.explicit_count(),
        state.dependency_count(),
        state.held_count(),
    ];
    for (index, filter) in InstalledFilter::ALL.iter().enumerate() {
        let label = filter.label();
        let width = text.measure(label, 12.0, Face::Ui) as i32 + 32;
        let pill = Rect::new(x, y, width, 28);
        let selected = state.installed_filter == *filter;
        let hovered = hits.hover_rect(state.last_pointer.0, state.last_pointer.1, pill);
        let color = ui::filter_pill(canvas, theme, pill, selected, hovered);
        let text_label = format!("{label} {}", counts[index]);
        let tw = text.measure(&text_label, 12.0, Face::Ui);
        text.draw(
            canvas,
            &text_label,
            (pill.x + pill.w / 2) as f32 - tw / 2.0,
            (pill.y + 18) as f32,
            12.0,
            color,
            Face::Ui,
        );
        hits.push(pill, Action::InstalledFilter(*filter));
        x += width + 8;
    }
    y += 42;

    let packages: Vec<StorePackage> = state
        .filtered_packages()
        .into_iter()
        .cloned()
        .collect();
    if packages.is_empty() {
        ui_cards::empty_state(
            canvas,
            text,
            theme,
            Rect::new(area.x, y, area.w, area.h - (y - area.y)),
            Icon::Folder,
            "这个筛选下没有软件包",
            "切换筛选条件，或在「分类浏览」里安装新软件。",
        );
        return;
    }

    let viewport = Rect::new(area.x, y, area.w, area.h - (y - area.y));
    let row_h = 52;
    let total_h = packages.len() as i32 * (row_h + 6);
    state.grid_scroll.set_bounds(
        viewport.h as f32,
        total_h as f32,
        Rect::new(area.x + area.w - 8, viewport.y, 8, viewport.h),
    );
    let offset = state.grid_scroll.offset_i32();
    for (index, pkg) in packages.iter().enumerate() {
        let row = Rect::new(
            area.x,
            y + index as i32 * (row_h + 6) - offset,
            area.w,
            row_h,
        );
        if row.y + row.h < viewport.y || row.y > viewport.y + viewport.h {
            continue;
        }
        let hovered = hits.hover_rect(state.last_pointer.0, state.last_pointer.1, row);
        ui::list_item_bg(canvas, theme, row, false, hovered);
        let pres = state.presentation(pkg);
        ui_cards::icon_badge(
            canvas,
            pres.icon,
            Rect::new(row.x + 10, row.y + 10, 32, 32),
            pres.accent,
        );
        ui_cards::draw_text_clipped(
            text,
            canvas,
            &pres.display_name,
            row.x + 54,
            row.y + 22,
            row.w - 360,
            13.0,
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
            row.x + 54,
            row.y + 40,
            row.w - 360,
            11.0,
            theme.text_muted,
            Face::Ui,
        );
        if pkg.held {
            let pill = Rect::new(row.x + row.w - 300, row.y + 15, 52, 22);
            ui_cards::badge(canvas, theme, pill, "锁定", theme.warning);
            ui_cards::draw_text(
                text,
                canvas,
                "锁定",
                pill.x + 12,
                pill.y + 15,
                11.0,
                theme.warning,
                Face::Ui,
            );
        }
        let hold = Rect::new(row.x + row.w - 236, row.y + 12, 64, 28);
        let hovered = hits.hover_rect(state.last_pointer.0, state.last_pointer.1, hold);
        ui_cards::cta_pill(
            canvas,
            text,
            theme,
            hold,
            if pkg.held { "解锁" } else { "锁定" },
            false,
            hovered,
            false,
            None,
        );
        hits.push(
            hold,
            if pkg.held {
                Action::Unhold(pkg.name.clone())
            } else {
                Action::Hold(pkg.name.clone())
            },
        );
        let reinstall = Rect::new(row.x + row.w - 164, row.y + 12, 64, 28);
        let hovered = hits.hover_rect(state.last_pointer.0, state.last_pointer.1, reinstall);
        ui_cards::cta_pill(
            canvas, text, theme, reinstall, "重装", false, hovered, false, None,
        );
        hits.push(reinstall, Action::Reinstall(pkg.name.clone()));
        let remove = Rect::new(row.x + row.w - 92, row.y + 12, 64, 28);
        let hovered = hits.hover_rect(state.last_pointer.0, state.last_pointer.1, remove);
        ui_cards::cta_pill(
            canvas, text, theme, remove, "卸载", false, hovered, false, None,
        );
        hits.push(remove, Action::Remove(pkg.name.clone()));
        hits.push(row, Action::OpenDetail(pkg.name.clone()));
    }
}

fn draw_detail(
    canvas: &mut Canvas,
    text: &mut TextRenderer,
    theme: &Theme,
    state: &mut StoreState,
    main: Rect,
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
    let area = content_scroll_area(state, main);
    let header = Rect::new(area.x, area.y, area.w, 150);
    ui::card(canvas, theme, header);
    ui_cards::icon_badge(
        canvas,
        pres.icon,
        Rect::new(header.x + 20, header.y + 20, 72, 72),
        pres.accent,
    );
    ui_cards::draw_text_clipped(
        text,
        canvas,
        &pres.display_name,
        header.x + 108,
        header.y + 40,
        header.w - 340,
        19.0,
        theme.text,
        Face::Bold,
    );
    ui_cards::draw_text_clipped(
        text,
        canvas,
        &pres.subtitle,
        header.x + 108,
        header.y + 62,
        header.w - 340,
        12.0,
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
        header.x + 108,
        header.y + 82,
        header.w - 340,
        11.5,
        theme.text_muted,
        Face::Ui,
    );

    let pointer = state.last_pointer;
    let mut button_x = header.x + 108;
    let primary_rect = Rect::new(button_x, header.y + 100, 118, 32);
    let hovered = hits.hover_rect(pointer.0, pointer.1, primary_rect);
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
        ui_cards::cta_pill(canvas, text, theme, primary_rect, "更新", true, hovered, false, None);
        hits.push(primary_rect, Action::Update(name.clone()));
    } else if pkg.installed {
        ui_cards::cta_pill(canvas, text, theme, primary_rect, "重新安装", false, hovered, false, None);
        hits.push(primary_rect, Action::Reinstall(name.clone()));
    } else {
        ui_cards::cta_pill(canvas, text, theme, primary_rect, "安装", true, hovered, false, None);
        hits.push(primary_rect, Action::Install(name.clone()));
    }
    button_x += 128;

    if pkg.installed {
        let remove = Rect::new(button_x, header.y + 100, 96, 32);
        let hovered = hits.hover_rect(pointer.0, pointer.1, remove);
        ui_cards::cta_pill(canvas, text, theme, remove, "卸载", false, hovered, false, None);
        hits.push(remove, Action::Remove(name.clone()));
        button_x += 106;

        let hold = Rect::new(button_x, header.y + 100, 96, 32);
        let hovered = hits.hover_rect(pointer.0, pointer.1, hold);
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
        button_x += 106;
    }
    if pres.is_theme {
        let appearance = Rect::new(button_x, header.y + 100, 132, 32);
        let hovered = hits.hover_rect(pointer.0, pointer.1, appearance);
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

    let stats = Rect::new(area.x, header.y + 162, area.w, 64);
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
            pkg.file_count.map(|n| n.to_string()).unwrap_or_else(|| "-".into()),
        ),
        (
            "来源",
            pkg.repo.clone().unwrap_or_else(|| state.repo_name.clone()),
        ),
    ];
    ui_cards::quick_stats(canvas, text, theme, stats, &items);

    let tabs = Rect::new(area.x, stats.y + 76, area.w, 32);
    let mut tab_x = tabs.x;
    for (tab, label) in [
        (crate::model::DetailTab::Overview, "概览"),
        (crate::model::DetailTab::Files, "文件列表"),
    ] {
        let width = text.measure(label, 12.5, Face::Ui) as i32 + 32;
        let rect = Rect::new(tab_x, tabs.y, width, 30);
        let active = state.detail_tab == tab;
        let hovered = hits.hover_rect(pointer.0, pointer.1, rect);
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
        tabs.y + 40,
        area.w,
        area.h - (tabs.y - area.y) - 48,
    );
    let mut y = body.y - state.detail_scroll.offset_i32();
    match state.detail_tab {
        crate::model::DetailTab::Overview => {
            for line in ui_cards::wrap_text(text, &pres.description, body.w as f32, 12.5, Face::Ui, 12)
            {
                ui_cards::draw_text_clipped(
                    text,
                    canvas,
                    &line,
                    body.x,
                    y + 12,
                    body.w,
                    12.5,
                    theme.text,
                    Face::Ui,
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
                    y + 12,
                    13.0,
                    theme.text,
                    Face::Bold,
                );
                y += 26;
                for highlight in &pres.highlights {
                    canvas.rounded_rect(
                        Rect::new(body.x, y + 6, 5, 5),
                        2,
                        theme.accent,
                        1.0,
                    );
                    for (index, line) in ui_cards::wrap_text(
                        text,
                        highlight,
                        (body.w - 18) as f32,
                        12.0,
                        Face::Ui,
                        2,
                    )
                    .into_iter()
                    .enumerate()
                    {
                        ui_cards::draw_text_clipped(
                            text,
                            canvas,
                            &line,
                            body.x + 16,
                            y + 12 + index as i32 * 18,
                            body.w - 18,
                            12.0,
                            theme.text_muted,
                            Face::Ui,
                        );
                    }
                    y += 26;
                }
            }
            if !pkg.depends.is_empty() {
                y += 8;
                ui_cards::draw_text(
                    text,
                    canvas,
                    "依赖",
                    body.x,
                    y + 12,
                    13.0,
                    theme.text,
                    Face::Bold,
                );
                y += 24;
                for dep in &pkg.depends {
                    let dep_name = dep.split(|c: char| c == '>' || c == '<' || c == '=').next().unwrap_or(dep);
                    let exists = state.package(dep_name).is_some();
                    let width = text.measure(dep, 12.0, Face::Ui) as i32 + 16;
                    let pill = Rect::new(body.x, y, width.max(60), 24);
                    ui_cards::badge(canvas, theme, pill, dep, theme.panel_border);
                    ui_cards::draw_text_clipped(
                        text,
                        canvas,
                        dep,
                        pill.x + 8,
                        pill.y + 16,
                        pill.w - 16,
                        12.0,
                        if exists { theme.accent } else { theme.text_muted },
                        Face::Ui,
                    );
                    if exists {
                        hits.push(pill, Action::OpenDetail(dep_name.to_string()));
                    }
                    y += 28;
                }
            }
        }
        crate::model::DetailTab::Files => {
            if state.detail_files.is_empty() {
                ui_cards::draw_text(
                    text,
                    canvas,
                    "尚未读取文件列表（仅已安装的软件包提供）。",
                    body.x,
                    y + 14,
                    12.5,
                    theme.text_muted,
                    Face::Ui,
                );
            }
            for file in &state.detail_files {
                ui_cards::draw_text_clipped(
                    text,
                    canvas,
                    &file.path,
                    body.x,
                    y + 14,
                    body.w - 120,
                    11.5,
                    theme.text,
                    Face::Mono,
                );
                ui_cards::draw_text_clipped(
                    text,
                    canvas,
                    &format!("{} · {}", file.file_type, format_bytes(file.size)),
                    body.x + body.w - 116,
                    y + 14,
                    116,
                    11.0,
                    theme.text_muted,
                    Face::Ui,
                );
                y += 20;
            }
        }
    }
    let total_h = (y + state.detail_scroll.offset_i32() - body.y).max(body.h) as f32;
    state.detail_scroll.set_bounds(
        body.h as f32,
        total_h,
        Rect::new(body.x + body.w - 8, body.y, 8, body.h),
    );
    ui::scrollbar_v_animated(
        canvas,
        theme,
        "store_detail_scrollbar",
        Rect::new(body.x + body.w - 8, body.y, 8, body.h),
        total_h,
        body.h as f32,
        state.detail_scroll.offset(),
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
    hits: &mut Hits,
) {
    let area = content_scroll_area(state, main);
    let pointer = state.last_pointer;
    let mut y = area.y;

    // repositories
    ui_cards::section_header(
        canvas,
        text,
        theme,
        Rect::new(area.x, y, area.w, 40),
        "软件源",
        "优先级越高越先被采用；至少保留一个启用的软件源。",
    );
    y += 46;
    for repo in state.repos.clone() {
        let row = Rect::new(area.x, y, area.w, 54);
        ui::card(canvas, theme, row);
        ui_cards::draw_text_clipped(
            text,
            canvas,
            &repo.name,
            row.x + 14,
            row.y + 22,
            row.w - 320,
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
            row.y + 40,
            row.w - 320,
            11.0,
            theme.text_muted,
            Face::Ui,
        );
        let up = Rect::new(row.x + row.w - 188, row.y + 13, 28, 28);
        let hovered = hits.hover_rect(pointer.0, pointer.1, up);
        ui_cards::cta_pill(canvas, text, theme, up, "+", false, hovered, false, None);
        hits.push(up, Action::RepoPriority(repo.name.clone(), 1));
        let down = Rect::new(row.x + row.w - 156, row.y + 13, 28, 28);
        let hovered = hits.hover_rect(pointer.0, pointer.1, down);
        ui_cards::cta_pill(canvas, text, theme, down, "−", false, hovered, false, None);
        hits.push(down, Action::RepoPriority(repo.name.clone(), -1));
        let remove = Rect::new(row.x + row.w - 116, row.y + 13, 100, 28);
        let hovered = hits.hover_rect(pointer.0, pointer.1, remove);
        ui_cards::cta_pill(canvas, text, theme, remove, "移除", false, hovered, false, None);
        hits.push(remove, Action::RepoRemove(repo.name.clone()));
        y += 62;
    }

    // add repo form
    let form = Rect::new(area.x, y, area.w, 82);
    ui::card(canvas, theme, form);
    let name_rect = Rect::new(form.x + 14, form.y + 14, 180, 30);
    ui::text_input_ime(
        canvas,
        text,
        theme,
        name_rect,
        &state.repo_name,
        "",
        "软件源名称",
        false,
        None,
    );
    let url_rect = Rect::new(form.x + 204, form.y + 14, form.w - 204 - 130, 30);
    ui::text_input_ime(
        canvas,
        text,
        theme,
        url_rect,
        &state.repo_url,
        "",
        "https://…",
        false,
        None,
    );
    let add = Rect::new(form.x + form.w - 116, form.y + 14, 100, 30);
    let hovered = hits.hover_rect(pointer.0, pointer.1, add);
    ui_cards::cta_pill(canvas, text, theme, add, "添加软件源", true, hovered, false, None);
    hits.push(add, Action::RepoAddSubmit);
    ui_cards::draw_text(
        text,
        canvas,
        "输入框在下方任务面板中通过「添加软件源」完成校验与写入。",
        form.x + 14,
        form.y + 66,
        11.0,
        theme.text_muted,
        Face::Ui,
    );
    y += 92;

    // trust keys
    ui_cards::section_header(
        canvas,
        text,
        theme,
        Rect::new(area.x, y, area.w, 40),
        "信任密钥",
        "Ed25519 公钥，用于校验仓库索引签名。",
    );
    y += 44;
    for key in &state.trust_keys {
        let row = Rect::new(area.x, y, area.w, 40);
        ui::list_item_bg(canvas, theme, row, false, false);
        icons::draw(canvas, Icon::Login, row.x + 10, row.y + 12, 16, Some(theme.success));
        ui_cards::draw_text_clipped(
            text,
            canvas,
            &key.key_id,
            row.x + 34,
            row.y + 24,
            row.w - 220,
            12.0,
            theme.text,
            Face::Mono,
        );
        ui_cards::draw_text_clipped(
            text,
            canvas,
            &key.comment,
            row.x + row.w - 180,
            row.y + 24,
            170,
            11.0,
            theme.text_muted,
            Face::Ui,
        );
        y += 44;
    }
    let trust_add = Rect::new(area.x, y + 4, 140, 28);
    let hovered = hits.hover_rect(pointer.0, pointer.1, trust_add);
    ui_cards::cta_pill(canvas, text, theme, trust_add, "导入信任密钥", false, hovered, false, None);
    hits.push(trust_add, Action::TrustAdd);
    y += 44;

    // cache stats + actions
    ui_cards::section_header(
        canvas,
        text,
        theme,
        Rect::new(area.x, y, area.w, 40),
        "缓存与一致性",
        "缓存只保存已下载的软件包，可随时清理。",
    );
    y += 44;
    let stats = Rect::new(area.x, y, area.w, 64);
    let cache_items: Vec<(&str, String)> = vec![
        ("仓库索引", format_bytes(state.cache.repository_bytes)),
        ("软件包", format_bytes(state.cache.package_bytes)),
        ("缓存包数", state.cache.package_count.to_string()),
        ("快照数", state.cache.snapshot_count.to_string()),
        ("临时分片", format_bytes(state.cache.part_bytes)),
    ];
    ui_cards::quick_stats(canvas, text, theme, stats, &cache_items);
    y += 76;

    let actions = [
        ("检查一致性", Action::MaintCheck),
        ("校验仓库", Action::MaintVerify),
        ("恢复事务", Action::MaintRepair),
        ("清理缓存", Action::CacheClean),
    ];
    let mut x = area.x;
    for (label, action) in actions {
        let width = text.measure(label, 12.0, Face::Bold) as i32 + 32;
        let rect = Rect::new(x, y, width, 30);
        let hovered = hits.hover_rect(pointer.0, pointer.1, rect);
        ui_cards::cta_pill(canvas, text, theme, rect, label, false, hovered, false, None);
        hits.push(rect, action);
        x += width + 8;
    }
    y += 44;

    if let Some(result) = state.maint.clone() {
        let panel = Rect::new(area.x, y, area.w, 140);
        ui::card(canvas, theme, panel);
        ui_cards::draw_text_clipped(
            text,
            canvas,
            &result.title,
            panel.x + 14,
            panel.y + 22,
            panel.w - 100,
            13.0,
            theme.text,
            Face::Bold,
        );
        ui_cards::badge(
            canvas,
            theme,
            Rect::new(panel.x + panel.w - 76, panel.y + 10, 62, 20),
            if result.ok { "通过" } else { "异常" },
            if result.ok { theme.success } else { theme.danger },
        );
        ui_cards::draw_text_clipped(
            text,
            canvas,
            &result.summary,
            panel.x + 14,
            panel.y + 44,
            panel.w - 28,
            11.5,
            theme.text_muted,
            Face::Ui,
        );
        let mut line_y = panel.y + 64;
        for line in result.details.iter().take(4) {
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
    }
}

fn draw_queue(
    canvas: &mut Canvas,
    text: &mut TextRenderer,
    theme: &Theme,
    state: &mut StoreState,
    rect: Rect,
    hits: &mut Hits,
) {
    canvas.fill(rect, theme.panel);
    canvas.line(rect.x, rect.y, rect.x + rect.w, rect.y, theme.panel_border, 1.0);
    let pointer = state.last_pointer;
    let mut x = rect.x + 16;
    for (tab, label) in [
        (crate::model::QueueTab::Jobs, "任务"),
        (crate::model::QueueTab::Console, "控制台"),
    ] {
        let width = text.measure(label, 12.5, Face::Bold) as i32 + 28;
        let tab_rect = Rect::new(x, rect.y + 8, width, 26);
        let active = state.queue_tab == tab;
        let hovered = hits.hover_rect(pointer.0, pointer.1, tab_rect);
        let color = ui::tab_item(canvas, theme, tab_rect, active, hovered, false, None);
        let tw = text.measure(label, 12.5, Face::Bold);
        text.draw(
            canvas,
            label,
            (tab_rect.x + tab_rect.w / 2) as f32 - tw / 2.0,
            (tab_rect.y + 18) as f32,
            12.5,
            color,
            Face::Bold,
        );
        hits.push(tab_rect, Action::QueueTab(tab));
        x += width + 6;
    }
    let close = Rect::new(rect.x + rect.w - 72, rect.y + 8, 56, 24);
    let hovered = hits.hover_rect(pointer.0, pointer.1, close);
    ui_cards::cta_pill(canvas, text, theme, close, "收起", false, hovered, false, None);
    hits.push(close, Action::ToggleQueue);

    let body = Rect::new(rect.x + 16, rect.y + 40, rect.w - 32, rect.h - 50);
    match state.queue_tab {
        crate::model::QueueTab::Jobs => {
            if state.jobs.is_empty() {
                ui_cards::draw_text(
                    text,
                    canvas,
                    "还没有任务。安装、更新或维护操作会出现在这里。",
                    body.x,
                    body.y + 16,
                    12.0,
                    theme.text_muted,
                    Face::Ui,
                );
            }
            let mut y = body.y;
            for job in state.jobs.iter().rev().take(4) {
                let row = Rect::new(body.x, y, body.w, 34);
                ui_cards::draw_text_clipped(
                    text,
                    canvas,
                    &format!("#{} {}", job.id, job.title),
                    row.x,
                    row.y + 14,
                    row.w - 320,
                    12.0,
                    theme.text,
                    Face::Bold,
                );
                ui_cards::draw_text_clipped(
                    text,
                    canvas,
                    &format!("{} · {}", job.status.label(), job.phase_msg),
                    row.x,
                    row.y + 28,
                    row.w - 320,
                    11.0,
                    theme.text_muted,
                    Face::Ui,
                );
                if job.status.is_active() {
                    ui::progress_bar(
                        canvas,
                        theme,
                        Rect::new(row.x + row.w - 300, row.y + 14, 180, 8),
                        job.percent as f32 / 100.0,
                        Some(theme.accent),
                    );
                    if job.status == TxJobStatus::Queued {
                        let cancel = Rect::new(row.x + row.w - 108, row.y + 6, 96, 24);
                        let hovered = hits.hover_rect(pointer.0, pointer.1, cancel);
                        ui_cards::cta_pill(
                            canvas, text, theme, cancel, "取消", false, hovered, false, None,
                        );
                        hits.push(cancel, Action::CancelJob(job.id));
                    }
                } else {
                    ui_cards::badge(
                        canvas,
                        theme,
                        Rect::new(row.x + row.w - 100, row.y + 8, 84, 20),
                        job.status.label(),
                        match job.status {
                            TxJobStatus::Succeeded => theme.success,
                            TxJobStatus::Failed(_) => theme.danger,
                            _ => theme.text_muted,
                        },
                    );
                }
                y += 38;
            }
        }
        crate::model::QueueTab::Console => {
            let mut y = body.y;
            for entry in state.logs.iter().rev().take(7).rev() {
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
                    y + 12,
                    body.w,
                    11.0,
                    color,
                    Face::Mono,
                );
                y += 16;
            }
        }
    }
}

fn draw_preview(
    canvas: &mut Canvas,
    text: &mut TextRenderer,
    theme: &Theme,
    content: Rect,
    preview: &PreviewState,
    hits: &mut Hits,
    pointer: (i32, i32),
) {
    let card = Rect::new(
        content.x + (content.w - 520) / 2,
        content.y + (content.h - 340) / 2,
        520,
        340,
    );
    ui_cards::modal_frame(canvas, theme, content, card);
    ui_cards::modal_title(
        canvas,
        text,
        theme,
        Rect::new(card.x + 20, card.y + 12, card.w - 40, 60),
        "确认事务",
        &preview.title,
    );
    let mut y = card.y + 84;
    match &preview.plan {
        Ok(plan) => {
            if plan.nothing_to_do {
                ui_cards::draw_text(
                    text,
                    canvas,
                    "没有需要执行的操作。",
                    card.x + 20,
                    y + 12,
                    12.5,
                    theme.text_muted,
                    Face::Ui,
                );
            }
            for action in plan.packages.iter().take(5) {
                let label = match action.from.as_deref() {
                    Some(from) => format!("{} {} → {}", action.name, from, action.to),
                    None => format!("{} {}", action.name, action.to),
                };
                ui_cards::draw_text_clipped(
                    text,
                    canvas,
                    &label,
                    card.x + 20,
                    y + 14,
                    card.w - 160,
                    12.0,
                    theme.text,
                    Face::Ui,
                );
                ui_cards::draw_text_clipped(
                    text,
                    canvas,
                    &format_bytes(action.size),
                    card.x + card.w - 120,
                    y + 14,
                    100,
                    11.0,
                    theme.text_muted,
                    Face::Ui,
                );
                y += 22;
            }
            for removal in plan.removals.iter().take(3) {
                ui_cards::draw_text_clipped(
                    text,
                    canvas,
                    &format!("移除 {} {}", removal.name, removal.version),
                    card.x + 20,
                    y + 14,
                    card.w - 40,
                    12.0,
                    theme.danger,
                    Face::Ui,
                );
                y += 22;
            }
            if !plan.held_back.is_empty() {
                ui_cards::draw_text_clipped(
                    text,
                    canvas,
                    &format!("{} 个软件包因版本锁定被保留", plan.held_back.len()),
                    card.x + 20,
                    y + 14,
                    card.w - 40,
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
                card.x + 20,
                y + 16,
                card.w - 40,
                12.0,
                theme.text_muted,
                Face::Ui,
            );
        }
        Err(error) => {
            ui_cards::draw_text_clipped(
                text,
                canvas,
                error,
                card.x + 20,
                y + 14,
                card.w - 40,
                12.0,
                theme.danger,
                Face::Ui,
            );
        }
    }

    if matches!(preview.job, TxJobKind::Remove { .. }) {
        let toggle = Rect::new(card.x + 20, card.y + card.h - 110, 160, 26);
        let _ = hits.hover_rect(pointer.0, pointer.1, toggle);
        ui::toggle_switch(canvas, theme, toggle, preview.recursive, false);
        ui_cards::draw_text(
            text,
            canvas,
            "同时移除无人依赖的依赖项",
            toggle.x + 44,
            toggle.y + 17,
            11.5,
            theme.text,
            Face::Ui,
        );
        hits.push(toggle, Action::ToggleRecursive);
        let purge = Rect::new(card.x + 20, card.y + card.h - 76, 160, 26);
        ui::toggle_switch(canvas, theme, purge, preview.purge, false);
        ui_cards::draw_text(
            text,
            canvas,
            "同时删除配置文件",
            purge.x + 44,
            purge.y + 17,
            11.5,
            theme.text,
            Face::Ui,
        );
        hits.push(purge, Action::TogglePurge);
    }

    let cancel = Rect::new(card.x + card.w - 208, card.y + card.h - 48, 92, 30);
    ui_cards::cta_pill(canvas, text, theme, cancel, "取消", false, false, false, None);
    hits.push(cancel, Action::PreviewCancel);
    let confirm = Rect::new(card.x + card.w - 108, card.y + card.h - 48, 92, 30);
    ui_cards::cta_pill(canvas, text, theme, confirm, "执行", true, false, false, None);
    hits.push(confirm, Action::PreviewConfirm);
}

fn draw_auth(
    canvas: &mut Canvas,
    text: &mut TextRenderer,
    theme: &Theme,
    content: Rect,
    auth: &AuthPrompt,
    hits: &mut Hits,
) -> Option<Rect> {
    let card = Rect::new(
        content.x + (content.w - 460) / 2,
        content.y + (content.h - 268) / 2,
        460,
        268,
    );
    ui_cards::modal_frame(canvas, theme, content, card);
    icons::draw(
        canvas,
        Icon::Login,
        card.x + 20,
        card.y + 20,
        24,
        Some(theme.accent),
    );
    ui_cards::modal_title(
        canvas,
        text,
        theme,
        Rect::new(card.x + 56, card.y + 8, card.w - 76, 60),
        "需要管理员授权",
        &format!("任务 #{} · {}", auth.job_id, auth.title),
    );
    ui_cards::draw_text_clipped(
        text,
        canvas,
        "将执行：",
        card.x + 20,
        card.y + 84,
        card.w - 40,
        11.5,
        theme.text_muted,
        Face::Ui,
    );
    ui_cards::draw_text_clipped(
        text,
        canvas,
        &format!("sudo {}", auth.command),
        card.x + 20,
        card.y + 102,
        card.w - 40,
        11.5,
        theme.text,
        Face::Mono,
    );
    let input = Rect::new(card.x + 20, card.y + 118, card.w - 40, 32);
    let masked: String = "•".repeat(auth.input.chars().count());
    let caret = ui::text_input_ime(
        canvas,
        text,
        theme,
        input,
        &masked,
        "",
        "输入管理员密码",
        true,
        Some(Icon::Login),
    );
    if let Some(error) = &auth.error {
        ui_cards::draw_text_clipped(
            text,
            canvas,
            error,
            card.x + 20,
            card.y + 170,
            card.w - 40,
            11.0,
            theme.danger,
            Face::Ui,
        );
    }
    let cancel = Rect::new(card.x + card.w - 208, card.y + card.h - 52, 92, 30);
    ui_cards::cta_pill(canvas, text, theme, cancel, "取消", false, false, false, None);
    hits.push(cancel, Action::AuthCancel);
    let confirm = Rect::new(card.x + card.w - 108, card.y + card.h - 52, 92, 30);
    ui_cards::cta_pill(canvas, text, theme, confirm, "授权", true, false, false, None);
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
    let alpha = 1.0 - (at.elapsed().as_secs_f32() - 3.0).max(0.0);
    let width = (text.measure(message, 12.0, Face::Ui) as i32 + 40).min(content.w - 80);
    let rect = Rect::new(
        content.x + (content.w - width) / 2,
        content.y + content.h - 56,
        width,
        34,
    );
    canvas.rounded_rect(rect, 8, theme.panel, alpha.min(1.0) * 0.96);
    canvas.rounded_border(rect, 8, theme.panel_border, alpha.min(1.0), 1);
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
