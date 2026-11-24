//! Tessera boot: fonts, configuration, window and the root view.

mod app;
mod keys;
mod terminal;
mod theme;

use std::borrow::Cow;

use gpui::{
    px, size, App, AppContext, Application, Bounds, TitlebarOptions, WindowBackgroundAppearance,
    WindowBounds, WindowOptions,
};

fn main() {
    Application::new().run(|cx: &mut App| {
        install_fonts(cx);

        let (mut config, warning) = tessera_core::config::Config::load();
        if let Some(warning) = warning {
            eprintln!("tessera: {warning}");
        }
        // Fall back to a family the platform actually has.
        let names = cx.text_system().all_font_names();
        if !names
            .iter()
            .any(|name| name.eq_ignore_ascii_case(&config.font_family))
        {
            let fallback = if cfg!(target_os = "macos") {
                "Menlo"
            } else if cfg!(windows) {
                "Consolas"
            } else {
                "DejaVu Sans Mono"
            };
            config.font_family = fallback.to_string();
        }

        let bounds = Bounds::centered(None, size(px(1080.0), px(680.0)), cx);
        let opened = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("Tessera".into()),
                    ..Default::default()
                }),
                window_background: WindowBackgroundAppearance::Opaque,
                app_id: Some("tessera".into()),
                window_min_size: Some(size(px(420.0), px(240.0))),
                ..Default::default()
            },
            move |window, cx| {
                window.set_window_title("Tessera");
                cx.new(|cx| app::TerminalApp::new(config, window, cx))
            },
        );
        if let Err(err) = opened {
            eprintln!("tessera: cannot open window: {err}");
            cx.quit();
            return;
        }
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        cx.activate(true);
    });
}

/// Bundle DejaVu Sans Mono so the terminal looks the same on machines without
/// it. Platforms whose text system cannot register fonts reject this; the
/// configured family is used instead.
fn install_fonts(cx: &mut App) {
    let fonts: Vec<Cow<'static, [u8]>> = vec![
        Cow::Borrowed(include_bytes!("../assets/DejaVuSansMono.ttf")),
        Cow::Borrowed(include_bytes!("../assets/DejaVuSansMono-Bold.ttf")),
        Cow::Borrowed(include_bytes!("../assets/DejaVuSansMono-Oblique.ttf")),
        Cow::Borrowed(include_bytes!("../assets/DejaVuSansMono-BoldOblique.ttf")),
    ];
    if let Err(err) = cx.text_system().add_fonts(fonts) {
        eprintln!("tessera: embedded fonts unavailable: {err}");
    }
}
