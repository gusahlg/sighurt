//! What the chrome draws in the content area itself: `sig://home` and error messages.

use gpui::prelude::*;
use gpui::{div, px, Div, FontWeight, SharedString};
use sighurt_core::config::Config;
use sighurt_core::page::command_found;

use crate::Colors;

/// One configured engine as the home page describes it.
pub(crate) struct EngineRow {
    name: SharedString,
    detail: SharedString,
    found: bool,
}

pub(crate) fn engine_rows(config: &Config) -> Vec<EngineRow> {
    config
        .engines
        .iter()
        .map(|engine| {
            let found = command_found(&engine.command);
            let mut detail = match (engine.command.is_empty(), found) {
                (true, _) => "no command".to_string(),
                (false, true) => format!("{} (found)", engine.command.join(" ")),
                (false, false) => format!("{} (not found)", engine.command.join(" ")),
            };
            if engine.name == config.default_engine {
                detail.push_str(" · default");
            }
            EngineRow {
                name: engine.name.clone().into(),
                detail: detail.into(),
                found,
            }
        })
        .collect()
}

/// The frame of the pages below, centred in the content area by [`centered`].
fn card(colors: &Colors) -> Div {
    div()
        .w(px(560.0))
        .p_5()
        .rounded_lg()
        .bg(colors.surface)
        .border_1()
        .border_color(colors.border)
}

fn centered(card: Div) -> Div {
    div()
        .flex_1()
        .flex()
        .items_center()
        .justify_center()
        .p_4()
        .child(card)
}

fn heading(text: &'static str) -> Div {
    div().text_xl().font_weight(FontWeight::BOLD).child(text)
}

pub(crate) fn home(engines: &[EngineRow], colors: &Colors) -> Div {
    let rows = engines.iter().map(|row| {
        div()
            .mt_2()
            .flex()
            .flex_row()
            .gap_3()
            .text_sm()
            .child(
                div()
                    .w(px(64.0))
                    .flex_shrink_0()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(row.name.clone()),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_color(if row.found {
                        colors.text_muted
                    } else {
                        colors.error
                    })
                    .child(row.detail.clone()),
            )
    });
    let hint = format!(
        "Type an address or a search above. Engines, search, keys and colors are set in {}.",
        Config::path().display()
    );
    centered(
        card(colors)
            .child(heading("Sighurt"))
            .child(
                div()
                    .mt_2()
                    .text_sm()
                    .text_color(colors.text_muted)
                    .child(hint),
            )
            .child(
                div()
                    .mt_4()
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(colors.text_dim)
                    .child("ENGINES"),
            )
            .children(rows),
    )
}

pub(crate) fn error(url: &str, message: &str, colors: &Colors) -> Div {
    centered(
        card(colors)
            .child(heading("Can't show this address"))
            .child(
                div()
                    .mt_2()
                    .text_xs()
                    .text_color(colors.text_dim)
                    .child(url.to_string()),
            )
            .child(
                div()
                    .mt_3()
                    .text_sm()
                    .text_color(colors.text_muted)
                    .child(message.to_string()),
            ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engine_rows_describe_each_engine() {
        let config = Config::parse(Some(
            "[engines.gone]\ncommand = [\"/nonexistent/sig-gone\"]\n[engines.none]\ncommand = []",
        ))
        .unwrap();
        let rows = engine_rows(&config);
        let row = |name: &str| rows.iter().find(|r| r.name.as_ref() == name).unwrap();
        assert!(!row("gone").found);
        assert_eq!(
            row("gone").detail.as_ref(),
            "/nonexistent/sig-gone (not found)"
        );
        assert_eq!(row("none").detail.as_ref(), "no command");
        assert!(row("servo").detail.ends_with("· default"));
    }
}
