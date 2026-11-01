/*
 *
 *       user/desktop/src/apps/help.rs
 *       SPUI 4.0 Help & System Manual: offline guide, shortcut index and architecture overview
 *
 *       2026/11/01 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Built-in offline documentation and manual viewer for SpaceOS 9.
//! Provides categorized guide pages covering desktop shortcuts, CLI utility
//! usage, `uxconfd` administration, and SpaceKernel architectural concepts.

use crate::app::{App, UiEvent};
use crate::draw::{Canvas, Rect};
use crate::icons::Icon;
use crate::text::{Face, TextRenderer};
use crate::theme::{Theme, RADIUS_CONTROL, TITLEBAR_HEIGHT};
use crate::ui;
use crate::x11::Result;

const SIDEBAR_W: i32 = 196;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Topic {
    Welcome,
    Shortcuts,
    CommandLine,
    UxConf,
    Architecture,
}

const TOPICS: [(Topic, &str); 5] = [
    (Topic::Welcome, "欢迎使用 SpaceOS 9"),
    (Topic::Shortcuts, "桌面与快捷键指南"),
    (Topic::CommandLine, "命令行与实用工具"),
    (Topic::UxConf, "系统配置 (uxconfd)"),
    (Topic::Architecture, "内核与系统架构"),
];

struct Section {
    title: &'static str,
    paragraphs: &'static [&'static str],
    bullets: &'static [&'static str],
    code_snippet: Option<&'static str>,
}

fn topic_content(topic: Topic) -> (&'static str, &'static [Section]) {
    match topic {
        Topic::Welcome => (
            "欢迎使用 SpaceOS 9 操作系统",
            &[
                Section {
                    title: "关于 SpaceOS 9",
                    paragraphs: &[
                        "SpaceOS 9 是由 ViudiraTech 研发的高性能自主操作系统，搭载 SpaceKernel 内核与自研原生 SPUI 4.0 图形桌面环境。",
                        "系统深度融合现代微内核设计理念与完备的 Linux ABI 兼容层，具备纳秒级 IPC、高吞吐内存文件系统与原生的 X11 桌面交互体验。",
                    ],
                    bullets: &[
                        "轻量原生：纯 Rust 构建桌面与窗口管理器，零重度框架包袱，内存占用极低。",
                        "完备套件：终端、文件管理器、文本编辑器、系统监视器、计算器、看图工具与系统设置一应俱全。",
                        "统一配置：搭载 uxconfd 配置中心，对标现代桌面标准，支持全局属性实时监听与热同步。",
                    ],
                    code_snippet: None,
                },
                Section {
                    title: "快速入门",
                    paragraphs: &[
                        "点击屏幕左下角的「开始」按钮或任务栏图标，即可快速启动各种预装应用程序。",
                        "右键点击桌面空白处，可呼出桌面快捷菜单，快速新建文件、打开终端或更换壁纸。",
                    ],
                    bullets: &[],
                    code_snippet: None,
                },
            ],
        ),
        Topic::Shortcuts => (
            "桌面操作与常用快捷键",
            &[
                Section {
                    title: "全局快捷操作",
                    paragraphs: &[
                        "SPUI 桌面支持丰富的键盘快捷键与高效鼠标手势操作：",
                    ],
                    bullets: &[
                        "双击窗口标题栏：默认最大化或还原当前窗口（可在「系统设置 -> 窗口」中自定义）。",
                        "按住标题栏拖拽：移动未最大化的窗口到任意位置。",
                        "拖拽窗口 8 边或 4 角：自由调整窗口宽度与高度。",
                        "鼠标滚轮滚动：在网页、终端、文件列表与看图工具中顺滑上下滑动。",
                    ],
                    code_snippet: None,
                },
                Section {
                    title: "应用内快捷键",
                    paragraphs: &[
                        "各内置原生应用的常用快捷键：",
                    ],
                    bullets: &[
                        "看图工具：左右方向键 (← / →) 快速切换上一张/下一张图片；鼠标滚轮无级缩放；R 键顺时针旋转 90 度；0 键重置适应窗口。",
                        "计算器：支持全物理键盘输入，包含小键盘数字、加减乘除、Enter 键求值与 Escape 键清空。",
                        "终端：Shift + PageUp / PageDown 在滚动缓冲区中上下翻页；Ctrl + Shift + C/V 复制粘贴。",
                    ],
                    code_snippet: None,
                },
            ],
        ),
        Topic::CommandLine => (
            "命令行实用工具链",
            &[
                Section {
                    title: "丰富终端生态",
                    paragraphs: &[
                        "SpaceOS 9 根文件系统预装了丰富完备的 Linux 命令行实用工具：",
                    ],
                    bullets: &[
                        "fastfetch：展示精美的 SpaceOS 标志与 CPU、内存、内核版本等系统全景概览。",
                        "htop：终端内高度互动的多核心性能监视器与进程管理器。",
                        "tree：层级树状打印文件目录结构。",
                        "file：精准探测与识别任意二进制或文档文件的 MIME 类型与格式。",
                        "nano：轻巧好用的终端全屏文本文档编辑器。",
                    ],
                    code_snippet: Some("fastfetch\nhtop\ntree -L 2 /usr/share/spui"),
                },
                Section {
                    title: "快速启动应用",
                    paragraphs: &[
                        "除了点击图形界面外，您也可以直接在终端命令行中调起任意 SPUI 桌面程序：",
                    ],
                    bullets: &[
                        "spui monitor：快速启动系统监视器 / 任务管理器",
                        "spui calc：快速启动桌面计算器",
                        "spui view <图片路径>：直接打开并预览指定图片",
                        "spui settings：打开系统偏好设置",
                    ],
                    code_snippet: Some("spui view /usr/share/spui/wallpapers/alpine-peaks.jpg &"),
                },
            ],
        ),
        Topic::UxConf => (
            "uxconfd 配置系统指南",
            &[
                Section {
                    title: "设计理念与体系",
                    paragraphs: &[
                        "uxconfd 是 SpaceOS 9 的集中式配置管理守护进程，基于 UNIX 域套接字 (/run/uxconfd.sock) 运行，采用层级 Channel + 斜杠属性路径管理配置（类似 XFCE xfconf）。",
                        "系统具备双层合并持久化能力：/etc/uxconf/channels/*.conf 保存系统出厂预设，/var/lib/uxconf/channels/*.conf 存储用户个性化更改，支持一键安全恢复默认。",
                    ],
                    bullets: &[
                        "appearance：管理深浅主题模式、强调色预设、界面字号缩放比例。",
                        "desktop & panel：管理桌面壁纸、填充方式、桌面图标显隐、面板位置与高度。",
                        "wm：管理窗口按钮左右布局、双击标题栏动作、焦点策略与新窗口初始位置。",
                        "display & input：管理屏幕分辨率、息屏时间、左右手鼠标、指针速度、自然滚动与按键重复。",
                    ],
                    code_snippet: None,
                },
                Section {
                    title: "使用 uxconf-query 命令行脚本控制",
                    paragraphs: &[
                        "您可以在脚本或终端中直接使用 uxconf-query 实时查询、修改与监听系统设置：",
                    ],
                    bullets: &[],
                    code_snippet: Some(
                        "# 列出所有配置频道\nuxconf-query -l\n\n\
                         # 查看外观频道的全部属性与类型\nuxconf-query -c appearance -l -v\n\n\
                         # 切换为浅色主题与翡翠强调色\nuxconf-query -c appearance -p /theme/mode -s light\n\
                         uxconf-query -c appearance -p /theme/accent -s '#10b981'\n\n\
                         # 实时监听系统中的所有配置变更广播\nuxconf-query -m"
                    ),
                },
            ],
        ),
        Topic::Architecture => (
            "SpaceKernel 架构与内核概览",
            &[
                Section {
                    title: "微内核与高性能子系统",
                    paragraphs: &[
                        "SpaceKernel 采用现代微内核思想设计，内核核心专注于提供极限速度的虚拟内存管理、per-CPU EEVDF 任务调度与轻量级进程间通信 (IPC)。",
                        "图形渲染架构采用纯软件 SWAR 并行混合引擎 + MIT-SHM 共享内存零拷贝传递，实现极低开销的高帧率桌面交互，不依赖庞大笨重的外部驱动栈。",
                    ],
                    bullets: &[
                        "多架构原生支持：一套代码库无缝适配 x86_64、AArch64 与 RISC-V 64 三大主流 CPU 指令集架构。",
                        "高可靠持久化：内置 ext4 与 JBD2 日志文件系统驱动，确保在断电或异常关机下磁盘数据依然严密安全。",
                        "现代化系统管理：由 systemg 作为 PID 1 统一编排系统服务，支持依赖解析、并行启动与崩溃自动拉起。",
                    ],
                    code_snippet: None,
                },
            ],
        ),
    }
}

pub fn run() -> Result<()> {
    let mut app = App::new("帮助手册", Icon::Help, 760, 500)?;
    app.min_width = 640;
    app.min_height = 420;

    let mut current_topic = Topic::Welcome;
    let mut scroll_y: i32 = 0;
    let mut hover_topic: Option<usize> = None;

    loop {
        if app.close {
            break;
        }

        if app.needs_redraw {
            let total_h = draw_help(
                &mut app.canvas,
                &mut app.text,
                &app.theme,
                current_topic,
                scroll_y,
                hover_topic,
                app.width as i32,
                app.height as i32,
            );
            let vp_h = (app.height as i32 - TITLEBAR_HEIGHT - 1).max(1);
            let max_scroll = (total_h - vp_h).max(0);
            if scroll_y > max_scroll {
                scroll_y = max_scroll;
            }
            app.draw_chrome();
            app.present()?;
        }

        for event in app.pump_with_fd(None, 60)?.0 {
            match event {
                UiEvent::Close => app.request_close(),
                UiEvent::Expose | UiEvent::Configure { .. } | UiEvent::ConfigChanged => {
                    app.needs_redraw = true;
                }
                UiEvent::Wheel { x, delta, .. } => {
                    if x >= SIDEBAR_W {
                        scroll_y = (scroll_y + delta * 20).max(0);
                        app.needs_redraw = true;
                    }
                }
                UiEvent::Motion { x, y, .. } => {
                    let next_hover = if x < SIDEBAR_W && y >= TITLEBAR_HEIGHT {
                        let idx = ((y - (TITLEBAR_HEIGHT + 24)) / 38) as usize;
                        if idx < TOPICS.len() {
                            Some(idx)
                        } else {
                            None
                        }
                    } else {
                        None
                    };
                    if next_hover != hover_topic {
                        hover_topic = next_hover;
                        app.needs_redraw = true;
                    }
                }
                UiEvent::Button {
                    x,
                    y,
                    button: 1,
                    down: true,
                    ..
                } if y >= TITLEBAR_HEIGHT => {
                    if x < SIDEBAR_W {
                        let idx = ((y - (TITLEBAR_HEIGHT + 24)) / 38) as usize;
                        if idx < TOPICS.len() {
                            current_topic = TOPICS[idx].0;
                            scroll_y = 0;
                            app.needs_redraw = true;
                        }
                    }
                }
                _ => {}
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn draw_help(
    canvas: &mut Canvas,
    text: &mut TextRenderer,
    theme: &Theme,
    topic: Topic,
    scroll_y: i32,
    hover: Option<usize>,
    w: i32,
    h: i32,
) -> i32 {
    canvas.clear(theme.window);

    // Left navigation sidebar
    let body_h = (h - TITLEBAR_HEIGHT).max(0);
    canvas.fill(
        Rect::new(0, TITLEBAR_HEIGHT, SIDEBAR_W, body_h),
        theme.window_alt,
    );
    canvas.fill(
        Rect::new(SIDEBAR_W - 1, TITLEBAR_HEIGHT, 1, body_h),
        theme.window_border,
    );

    text.draw(
        canvas,
        "使用指南",
        18.0,
        (TITLEBAR_HEIGHT + 22) as f32,
        11.0,
        theme.text_muted,
        Face::Bold,
    );

    for (i, (t, label)) in TOPICS.iter().enumerate() {
        let ry = TITLEBAR_HEIGHT + 36 + i as i32 * 38;
        let r = Rect::new(10, ry, SIDEBAR_W - 20, 32);
        let active = topic == *t;
        if active {
            canvas.rounded_rect(r, RADIUS_CONTROL, theme.accent_soft, 1.0);
            canvas.fill(Rect::new(r.x, r.y + 6, 3, r.h - 12), theme.accent);
        } else if hover == Some(i) {
            canvas.rounded_rect(r, RADIUS_CONTROL, theme.panel_hover, 1.0);
        }
        text.draw(
            canvas,
            label,
            (r.x + 14) as f32,
            (r.y + 20) as f32,
            12.0,
            if active { theme.text } else { theme.text_dim },
            if active { Face::Bold } else { Face::Ui },
        );
    }

    // Right content viewport
    let content_x = SIDEBAR_W + 28;
    let content_w = (w - content_x - 24).max(360);
    let vp = Rect::new(SIDEBAR_W, TITLEBAR_HEIGHT, w - SIDEBAR_W, body_h);

    let (heading, sections) = topic_content(topic);
    let mut total_h = 0;

    canvas.with_clip(vp, |c| {
        let mut y = TITLEBAR_HEIGHT + 28 - scroll_y;

        // Big Heading
        text.draw(
            c,
            heading,
            content_x as f32,
            (y + 18) as f32,
            18.0,
            theme.text,
            Face::Bold,
        );
        y += 36;
        c.fill(Rect::new(content_x, y, content_w, 1), theme.window_border);
        y += 20;

        for sec in sections {
            // Section Title
            text.draw(
                c,
                sec.title,
                content_x as f32,
                (y + 14) as f32,
                14.0,
                theme.text,
                Face::Bold,
            );
            y += 26;

            // Paragraphs
            for p in sec.paragraphs {
                let lines = wrap_text(text, p, content_w as f32, 12.0);
                for line in lines {
                    text.draw(
                        c,
                        &line,
                        content_x as f32,
                        (y + 14) as f32,
                        12.0,
                        theme.text_dim,
                        Face::Ui,
                    );
                    y += 20;
                }
                y += 6;
            }

            // Bullets
            for b in sec.bullets {
                c.circle(
                    (content_x + 6) as f32,
                    (y + 10) as f32,
                    2.5,
                    theme.accent,
                    1.0,
                );
                let lines = wrap_text(text, b, (content_w - 20) as f32, 12.0);
                for (l_idx, line) in lines.into_iter().enumerate() {
                    let bx = if l_idx == 0 {
                        content_x + 18
                    } else {
                        content_x + 18
                    };
                    text.draw(
                        c,
                        &line,
                        bx as f32,
                        (y + 14) as f32,
                        12.0,
                        theme.text,
                        Face::Ui,
                    );
                    y += 20;
                }
                y += 4;
            }

            // Code snippet card
            if let Some(code) = sec.code_snippet {
                y += 6;
                let code_lines: Vec<&str> = code.lines().collect();
                let card_h = code_lines.len() as i32 * 20 + 20;
                let card_r = Rect::new(content_x, y, content_w, card_h);
                c.rounded_rect(card_r, RADIUS_CONTROL, theme.input_bg, 1.0);
                c.rounded_border(card_r, RADIUS_CONTROL, theme.panel_border, 1.0, 1);
                for (c_idx, c_line) in code_lines.into_iter().enumerate() {
                    let ly = card_r.y + 18 + c_idx as i32 * 20;
                    let is_comment = c_line.starts_with('#');
                    let fg = if is_comment {
                        theme.text_muted
                    } else {
                        theme.text
                    };
                    text.draw(
                        c,
                        c_line,
                        (card_r.x + 14) as f32,
                        ly as f32,
                        11.5,
                        fg,
                        Face::Mono,
                    );
                }
                y += card_h + 16;
            } else {
                y += 14;
            }
        }

        total_h = (y + scroll_y - (TITLEBAR_HEIGHT + 28)).max(vp.h);
    });

    // Scrollbar
    if total_h > vp.h {
        let track = Rect::new(w - 8, vp.y + 4, 6, vp.h - 8);
        ui::scrollbar_v(
            canvas,
            theme,
            track,
            total_h as usize,
            vp.h as usize,
            scroll_y as usize,
        );
    }

    total_h
}

fn wrap_text(text: &mut TextRenderer, input: &str, max_w: f32, font_size: f32) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();

    for ch in input.chars() {
        cur.push(ch);
        if text.measure(&cur, font_size, Face::Ui) > max_w {
            cur.pop();
            if !cur.is_empty() {
                out.push(cur.clone());
            }
            cur.clear();
            cur.push(ch);
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}
