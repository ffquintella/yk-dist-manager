//! egui screens. Each module exposes a single `show` entry point and owns no
//! state of its own — everything lives on [`crate::app::YkDistApp`].
//!
//! The look comes from [`egui-elegance`](elegance): [`install_theme`] is called
//! once per frame from the shell, which restyles the stock egui widgets and
//! lets the elegance widgets pick the palette up. Everything below is the small
//! set of building blocks the screens share, so a heading, a table header or a
//! refusal looks the same on all of them.
//!
//! # Fluid layout
//!
//! The screens are fluid: one [`GUTTER`] on each side of the window, and every
//! card, banner and form spanning what is left, whatever the window width. That
//! is what [`card`], [`titled_card`], [`table`] and [`form_columns`] are for —
//! a bare `elegance::Card` is an `egui::Frame`, which hugs its contents, so
//! cards built by hand end up as wide as whatever happens to be inside them and
//! no two screens line up. Width belongs to the layout, not to the content:
//! fields take the width of their column instead of a constant, and a table too
//! wide for the window scrolls sideways *inside its own card* rather than
//! stretching the page.

use elegance::{
    Accent, Badge, BadgeTone, Button, Callout, CalloutTone, Card, TextArea, TextInput, Theme,
};

use crate::domain::KeyStatus;

pub mod audit;
pub mod bootstrap;
pub mod database;
pub mod distribution;
pub mod holders;
pub mod inventory;
pub mod reports;
pub mod settings;
pub mod templates;
pub mod terms;

/// Space between the window edge and the content, applied by the shell to the
/// top bar, the screen body and the status bar — so the product name, the
/// screen heading and the status pill all sit on one left margin.
pub const GUTTER: i8 = 18;

/// Horizontal gap between two form columns.
const COLUMN_GAP: f32 = 20.0;

/// Column and row spacing shared by every table, so the tables on different
/// screens read as the same table.
const TABLE_SPACING: [f32; 2] = [18.0, 8.0];

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

/// A card that spans the page width.
pub fn card<R>(ui: &mut egui::Ui, body: impl FnOnce(&mut egui::Ui) -> R) -> R {
    stretch(Card::new(), ui, body)
}

/// A card with a caption, spanning the page width.
pub fn titled_card<R>(
    ui: &mut egui::Ui,
    heading: impl Into<egui::WidgetText>,
    body: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    stretch(Card::new().heading(heading), ui, body)
}

/// Paint a card that claims the whole row.
///
/// `Card` is an [`egui::Frame`], which sizes itself to its contents; claiming
/// the available width is what keeps a two-field form and a seven-column table
/// the same width on screen.
fn stretch<R>(card: Card, ui: &mut egui::Ui, body: impl FnOnce(&mut egui::Ui) -> R) -> R {
    card.show(ui, |ui| {
        ui.set_min_width(ui.available_width());
        body(ui)
    })
    .inner
}

/// A table: striped, one spacing, one header style, and its own horizontal
/// scroll.
///
/// The scroll is the point. A serial and a 64-character digest do not fit the
/// same window, and a table that pushed the page wider would take every other
/// card with it — so the overflow is contained here, and the cards around it
/// stay the width of the window.
pub fn table<R>(
    ui: &mut egui::Ui,
    id: &str,
    headers: &[&str],
    rows: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    let columns = headers.len();
    egui::ScrollArea::horizontal()
        .id_salt(format!("{id}-scroll"))
        .show(ui, |ui| {
            egui::Grid::new(id)
                .striped(true)
                .num_columns(columns)
                .spacing(TABLE_SPACING)
                .show(ui, |ui| {
                    table_header(ui, headers);
                    rows(ui)
                })
                .inner
        })
        .inner
}

/// Two form columns that split the page width evenly.
///
/// The closure gets both columns and the width each one has. The width is passed
/// because [`elegance::Select`] falls back to a constant when it is not told
/// one, while [`TextInput`] and [`TextArea`] already default to the space they
/// are given — so most fields need no width at all and grow with the window.
///
/// One closure rather than two, so a form can drive both columns from the same
/// `&mut app`. The unconditional split is safe because the window has a 900px
/// minimum (`main.rs`): half of that is still a readable field.
pub fn form_columns(
    ui: &mut egui::Ui,
    add_contents: impl FnOnce(&mut egui::Ui, &mut egui::Ui, f32),
) {
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.x = COLUMN_GAP;
        ui.columns(2, |columns| {
            let width = columns[0].available_width();
            let (left, right) = columns.split_at_mut(1);
            add_contents(&mut left[0], &mut right[0], width);
        });
    });
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
            // Full width, like a card and like the elegance callout next to it.
            ui.set_min_width(ui.available_width());
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
                    // A refusal is a sentence, and the page no longer scrolls
                    // sideways to accommodate one: wrap inside the frame.
                    .wrap_mode(egui::TextWrapMode::Wrap)
                    .selectable(true),
                );
            });
        });
}

/// The application mark, at `side` logical pixels
/// (`features/application-icon.md` phase 7).
///
/// The texture is uploaded once and kept in **egui's** own memory rather than on
/// [`crate::app::YkDistApp`]. Two reasons, and the second is the one that decides
/// it: a texture handle is a paint-layer resource with no business on the
/// application state, and `load_texture` on every frame would upload 256 KiB to the
/// GPU sixty times a second.
///
/// Draws nothing at all if the embedded blob is malformed. The mark is decoration:
/// the screen it sits on has to work without it.
pub fn app_icon(ui: &mut egui::Ui, side: f32) {
    let key = egui::Id::new("app-icon-texture");

    // Three steps, and the shape is load-bearing: the lookup and the insert each
    // take egui's data lock **briefly**, and `load_texture` runs between them with
    // no lock held. Uploading from inside `data_mut` would take the same context
    // lock twice and hang the frame — a deadlock, not a slow path, and one that only
    // appears when the texture is missing, which is the first frame.
    let cached: Option<egui::TextureHandle> = ui.ctx().data(|data| data.get_temp(key));

    let handle = match cached {
        Some(handle) => Some(handle),
        None => match crate::branding::icon_image() {
            Some(image) => {
                let handle = ui
                    .ctx()
                    .load_texture("app-icon", image, egui::TextureOptions::LINEAR);
                ui.ctx()
                    .data_mut(|data| data.insert_temp(key, handle.clone()));
                Some(handle)
            }
            None => None,
        },
    };

    if let Some(handle) = handle {
        ui.add(egui::Image::new(&handle).fit_to_exact_size(egui::vec2(side, side)));
    }
}

/// The database-password strength meter
/// (`features/db-password-and-encryption.md` phase 6).
///
/// Shared by the chooser and by Settings, so the two places a password can be
/// *chosen* grade it identically — and grade it with
/// [`crate::password::assess`], which is also what the store enforces. A meter
/// that disagreed with the refusal would be worse than no meter.
///
/// Never a bar of colour on its own: the bar carries the word
/// ([`crate::password::Strength::label`]), a refusal is printed as a refusal, and
/// the advice is printed underneath. That is the `gui-shell` phase 10 rule, and
/// here it is also the useful behaviour — "weak" tells an operator nothing about
/// what to do, "several unrelated words work well" does.
pub fn password_meter(ui: &mut egui::Ui, password: &str) -> crate::password::Assessment {
    use crate::password::Strength;

    let assessment = crate::password::assess(password);
    let accent = match assessment.strength {
        Strength::TooWeak => Accent::Red,
        Strength::Weak => Accent::Amber,
        Strength::Fair => Accent::Sky,
        Strength::Strong => Accent::Green,
    };

    ui.add(
        elegance::ProgressBar::new(assessment.strength.fraction())
            .accent(accent)
            .text(assessment.strength.label()),
    );
    for refusal in &assessment.refusals {
        ui.add_space(6.0);
        error_label(ui, refusal);
    }
    for advice in &assessment.advice {
        ui.add_space(4.0);
        hint(ui, advice);
    }
    assessment
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

/// A search box plus the paging controls, drawn above a table.
///
/// Returns true when the operator changed something that should reset the page.
/// The *rules* live in [`crate::browse`] — this only collects what was typed and
/// clicked, so the untestable part stays as thin as the coverage contract in
/// `AGENTS.md` §4 requires.
pub fn table_controls<S: Copy + PartialEq>(
    ui: &mut egui::Ui,
    browse: &mut crate::app::Browse<S>,
    pages: usize,
    page: usize,
    summary: &str,
) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label("Search");
        let response = ui.add(
            egui::TextEdit::singleline(&mut browse.query)
                .hint_text("serial, name, e-mail, note…")
                .desired_width(280.0),
        );
        if response.changed() {
            // A new query makes the current page meaningless — and landing on an
            // empty page after typing is how an operator concludes a key is not
            // in the register.
            browse.page = 0;
            changed = true;
        }
        if !browse.query.is_empty() && ui.button("Clear").clicked() {
            browse.query.clear();
            browse.page = 0;
            changed = true;
        }

        ui.add_space(12.0);
        if pages > 1 {
            // Enabled-state, not just colour: a disabled button reads as
            // disabled to a screen reader too (`gui-shell` phase 10).
            if ui
                .add_enabled(page > 0, egui::Button::new("◀ Previous"))
                .clicked()
            {
                browse.page = browse.page.saturating_sub(1);
                changed = true;
            }
            if ui
                .add_enabled(page + 1 < pages, egui::Button::new("Next ▶"))
                .clicked()
            {
                browse.page += 1;
                changed = true;
            }
        }
        ui.add_space(12.0);
        ui.label(summary);
    });
    changed
}

/// A clickable column header that sorts by its column.
///
/// The arrow is text rather than colour, so the sorted column is identifiable
/// without seeing hue — `features/gui-shell.md` phase 10.
pub fn sort_header<S: Copy + PartialEq>(
    ui: &mut egui::Ui,
    browse: &mut crate::app::Browse<S>,
    label: &str,
    column: S,
) {
    let marker = if browse.sort == column {
        browse.direction.arrow()
    } else {
        ""
    };
    let text = if marker.is_empty() {
        label.to_owned()
    } else {
        format!("{label} {marker}")
    };
    if ui
        .add(egui::Button::new(egui::RichText::new(text).strong()).frame(false))
        .on_hover_text(format!("Sort by {label}"))
        .clicked()
    {
        browse.sort_by(column);
    }
}
