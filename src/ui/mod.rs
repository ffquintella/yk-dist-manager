//! egui screens. Each module exposes a single `show` entry point and owns no
//! state of its own — everything lives on [`crate::app::YkDistApp`].
//!
//! The look comes from [`egui-elegance`](elegance): [`install_theme`] is called
//! once per frame from the shell, which restyles the stock egui widgets and
//! lets the elegance widgets pick the palette up. Everything below is the small
//! set of building blocks the screens share, so a heading, a table header or a
//! refusal looks the same on all of them.

use elegance::{
    Accent, Badge, BadgeTone, Button, Callout, CalloutTone, TextArea, TextInput, Theme,
};

use crate::domain::KeyStatus;

pub mod audit;
pub mod bootstrap;
pub mod database;
pub mod distribution;
pub mod holders;
pub mod inventory;
pub mod settings;
pub mod terms;

/// Install the operator's chosen palette.
///
/// Cheap enough to call every frame: `Theme::install` compares against the
/// theme already in context memory and skips the style write when it matches.
pub fn install_theme(ctx: &egui::Context, name: &str) {
    theme_named(name).install(ctx);
}

/// Map a settings palette name to its elegance theme. Unknown names have
/// already been resolved by [`crate::settings::normalise_theme`]; the match is
/// exhaustive here so an unhandled name still paints rather than panics.
fn theme_named(name: &str) -> Theme {
    match name {
        "charcoal" => Theme::charcoal(),
        "frost" => Theme::frost(),
        "paper" => Theme::paper(),
        _ => Theme::slate(),
    }
}

/// Heading plus explanatory line, used at the top of every screen.
pub fn screen_header(ui: &mut egui::Ui, title: &str, subtitle: &str) {
    let theme = Theme::current(ui.ctx());
    ui.add(egui::Label::new(
        egui::RichText::new(title)
            .size(theme.typography.heading + 6.0)
            .color(theme.palette.text)
            .strong(),
    ));
    ui.add_space(2.0);
    ui.add(egui::Label::new(
        egui::RichText::new(subtitle)
            .size(theme.typography.label)
            .color(theme.palette.text_muted),
    ));
    ui.add_space(12.0);
}

/// Small, muted explanatory text — the "why this field exists" line that sits
/// under a control.
pub fn hint(ui: &mut egui::Ui, text: &str) {
    let theme = Theme::current(ui.ctx());
    ui.add(egui::Label::new(
        egui::RichText::new(text)
            .size(theme.typography.small)
            .color(theme.palette.text_faint),
    ));
}

/// Red, selectable error text — errors go to the screen *and* the log, never
/// only to a dialog that vanishes.
///
/// Deliberately not an [`elegance::Callout`]: a callout paints its body text,
/// and AGENTS.md requires a refusal the operator can select and copy into a
/// ticket. This reproduces the callout's tinted-danger frame around a real
/// selectable [`egui::Label`].
pub fn error_label(ui: &mut egui::Ui, message: &str) {
    let theme = Theme::current(ui.ctx());
    let danger = theme.palette.danger;
    // The same 10% fill / 28% border tint the elegance callout uses.
    let fill = egui::Color32::from_rgba_unmultiplied(danger.r(), danger.g(), danger.b(), 26);
    let stroke = egui::Color32::from_rgba_unmultiplied(danger.r(), danger.g(), danger.b(), 71);

    egui::Frame::new()
        .fill(fill)
        .stroke(egui::Stroke::new(1.0, stroke))
        .corner_radius(egui::CornerRadius::same(8))
        .inner_margin(egui::Margin {
            left: 14,
            right: 14,
            top: 10,
            bottom: 10,
        })
        .show(ui, |ui| {
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                ui.add(
                    egui::Label::new(
                        egui::RichText::new("!")
                            .strong()
                            .color(danger)
                            .size(theme.typography.body + 1.0),
                    )
                    .wrap_mode(egui::TextWrapMode::Extend),
                );
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(message)
                            .color(theme.palette.text)
                            .size(theme.typography.body),
                    )
                    .selectable(true),
                );
            });
        });
}

/// A non-error banner: a condition worth noticing, or a build that is missing
/// an optional feature.
pub fn notice(ui: &mut egui::Ui, tone: CalloutTone, message: &str) {
    Callout::new(tone)
        .tinted()
        .multiline()
        .body(message)
        .show(ui, |_| {});
}

/// A length-capped single-line input.
///
/// The cap is the point: `elegance::TextInput` has no `char_limit`, so the
/// bound NRM §5.3.5 asks for is applied here, immediately after the field is
/// painted. `build` receives the input so the call site can add a hint, a
/// width or password masking.
pub fn capped_input(
    ui: &mut egui::Ui,
    value: &mut String,
    max: usize,
    build: impl for<'a> FnOnce(TextInput<'a>) -> TextInput<'a>,
) -> egui::Response {
    let response = ui.add(build(TextInput::new(value)));
    crate::domain::clamp_text(value, max);
    response
}

/// A length-capped multi-line input. See [`capped_input`].
pub fn capped_area(
    ui: &mut egui::Ui,
    value: &mut String,
    max: usize,
    build: impl for<'a> FnOnce(TextArea<'a>) -> TextArea<'a>,
) -> egui::Response {
    let response = ui.add(build(TextArea::new(value)));
    crate::domain::clamp_text(value, max);
    response
}

/// Header row of a table: muted, small, and the same on every screen.
pub fn table_header(ui: &mut egui::Ui, headers: &[&str]) {
    let theme = Theme::current(ui.ctx());
    for header in headers {
        ui.add(egui::Label::new(
            egui::RichText::new(*header)
                .size(theme.typography.small)
                .color(theme.palette.text_muted)
                .strong(),
        ));
    }
    ui.end_row();
}

/// Monospaced cell — serials, e-mails, paths and digests, the values an
/// operator compares character by character.
pub fn mono(ui: &mut egui::Ui, text: &str) -> egui::Response {
    let theme = Theme::current(ui.ctx());
    ui.add(
        egui::Label::new(
            egui::RichText::new(text)
                .monospace()
                .size(theme.typography.monospace)
                .color(theme.palette.text),
        )
        .selectable(true),
    )
}

/// Muted cell text — details and captions inside a table.
pub fn faint(ui: &mut egui::Ui, text: &str) {
    let theme = Theme::current(ui.ctx());
    ui.add(egui::Label::new(
        egui::RichText::new(text)
            .size(theme.typography.small)
            .color(theme.palette.text_faint),
    ));
}

/// A key's lifecycle state as a badge. The tones carry the meaning: a lost key
/// is red because it needs revoking, a retired one is grey because it needs
/// nothing.
pub fn status_badge(ui: &mut egui::Ui, status: KeyStatus) {
    let tone = match status {
        KeyStatus::InStock => BadgeTone::Neutral,
        KeyStatus::Bootstrapped => BadgeTone::Info,
        KeyStatus::Distributed => BadgeTone::Ok,
        KeyStatus::Returned => BadgeTone::Warning,
        KeyStatus::Lost => BadgeTone::Danger,
        KeyStatus::Retired => BadgeTone::Neutral,
    };
    ui.add(Badge::new(status.label(), tone));
}

/// A small secondary action inside a table row.
pub fn row_button(ui: &mut egui::Ui, label: &str) -> egui::Response {
    ui.add(
        Button::new(label)
            .outline()
            .size(elegance::ButtonSize::Small),
    )
}

/// A small destructive action inside a table row.
pub fn row_button_danger(ui: &mut egui::Ui, label: &str) -> egui::Response {
    ui.add(
        Button::new(label)
            .accent(Accent::Red)
            .size(elegance::ButtonSize::Small),
    )
}
