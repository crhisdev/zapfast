//! The Status tab, laid out as WhatsApp's: the people who posted in the last
//! day on the left, newest first and those not yet seen above the rest, and
//! the status picked from them on the right, one at a time with a bar per
//! status of its author. Text and pictures move on by themselves; videos
//! wait for the reader.

use std::collections::HashSet;

use egui::{Align, Color32, Layout, Rect, Sense, Vec2, pos2, vec2};

use crate::app::App;
use crate::i18n::gettext;
use crate::model::{Action, Content, MediaState, Message, Page, STATUS_CHAT};
use crate::theme::{self, Icon};
use crate::ui::widgets;

/// Width of the list of authors beside the viewer.
const LIST_WIDTH: f32 = 340.0;
/// How long a text or picture status stays before the next one.
const STATUS_SECONDS: f64 = 6.0;
/// The viewer's backdrop, dark in either theme as WhatsApp's is.
const BACKDROP: Color32 = Color32::from_rgb(11, 20, 26);
/// A text status's card, WhatsApp's default status green.
const TEXT_CARD: Color32 = Color32::from_rgb(0, 92, 75);

/// One person's statuses, oldest first, as the list shows them.
#[derive(Debug)]
pub struct Author<'a> {
    pub id: &'a str,
    pub from_me: bool,
    pub statuses: Vec<&'a Message>,
    /// Whether any of them has not been seen here yet.
    pub unseen: bool,
}

impl Author<'_> {
    fn latest(&self) -> i64 {
        self.statuses.last().map_or(0, |status| status.timestamp)
    }

    /// The status the author's row opens on: the first not yet seen.
    fn first_unseen(&self, seen: &HashSet<String>) -> &str {
        self.statuses
            .iter()
            .find(|status| !seen.contains(&status.id))
            .or(self.statuses.first())
            .map_or("", |status| status.id.as_str())
    }
}

/// `statuses` (oldest first) by author: our own first, then those with
/// something unseen, then the rest, each group newest first.
pub fn authors<'a>(statuses: &'a [Message], seen: &HashSet<String>) -> Vec<Author<'a>> {
    let mut authors: Vec<Author<'a>> = Vec::new();
    for status in statuses {
        match authors.iter_mut().find(|author| author.id == status.sender) {
            Some(author) => author.statuses.push(status),
            None => authors.push(Author {
                id: &status.sender,
                from_me: status.from_me,
                statuses: vec![status],
                unseen: false,
            }),
        }
    }
    for author in &mut authors {
        author.unseen = !author.from_me
            && author
                .statuses
                .iter()
                .any(|status| !seen.contains(&status.id));
    }
    authors.sort_by_key(|author| {
        (
            !author.from_me,
            !author.unseen,
            std::cmp::Reverse(author.latest()),
        )
    });
    authors
}

/// Stable id of an author's row, for tests.
pub fn row_id(author: &str) -> egui::Id {
    egui::Id::new(("status-row", author))
}

/// The Status page: the list beside the viewer, or one of them in a
/// narrow window.
pub fn show(app: &mut App, ui: &mut egui::Ui) {
    let rect = ui.max_rect();
    let open = app
        .status_open
        .clone()
        .filter(|(_, id)| app.statuses.iter().any(|status| &status.id == id));
    if super::narrow(ui.ctx()) {
        match open {
            Some((author, id)) => viewer(app, ui, rect, &author, &id),
            None => list(app, ui, rect, true),
        }
        return;
    }
    let width = LIST_WIDTH.min(rect.width() * 0.45);
    let (left, right) = rect.split_left_right_at_x(rect.left() + width);
    list(app, ui, left, false);
    ui.painter().vline(
        left.right(),
        left.y_range(),
        egui::Stroke::new(1.0, app.palette.outline),
    );
    match open {
        Some((author, id)) => viewer(app, ui, right, &author, &id),
        None => placeholder(app, ui, right),
    }
}

/// The people who posted, under the page's title.
fn list(app: &mut App, ui: &mut egui::Ui, rect: Rect, narrow: bool) {
    let palette = app.palette;
    let locale = app.locale;
    let room = super::caption_room(
        ui.ctx(),
        Rect::from_min_size(rect.min, vec2(rect.width(), 44.0)),
        160.0,
    );
    let mut ui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(Layout::top_down(Align::Min)),
    );
    ui.add_space(12.0 + room.top);
    ui.horizontal(|ui| {
        ui.add_space(12.0 + room.left);
        if narrow
            && theme::icon_button(
                ui,
                Icon::ArrowLeft,
                20.0,
                palette.secondary,
                palette.text,
                &format!("{} (Esc)", gettext(locale, "Back")),
            )
            .clicked()
        {
            app.actions.push(Action::Open(Page::Chats));
        }
        ui.add_space(4.0);
        theme::text(
            ui,
            gettext(locale, "Status"),
            theme::bold(22.0),
            palette.text,
        );
    });
    ui.add_space(8.0);
    let seen = app.statuses_seen.clone();
    let statuses = app.statuses.clone();
    let authors = authors(&statuses, &seen);
    if authors.is_empty() {
        let (title, body) = if app.statuses_loaded {
            (
                gettext(locale, "No status updates"),
                gettext(
                    locale,
                    "Statuses your contacts post show here for 24 hours.",
                ),
            )
        } else {
            (gettext(locale, "Loading…"), Default::default())
        };
        ui.add_space(40.0);
        ui.vertical_centered(|ui| {
            theme::text(ui, title, theme::semibold(15.0), palette.text);
            ui.add_space(6.0);
            ui.add(
                egui::Label::new(
                    egui::RichText::new(body)
                        .font(theme::regular(13.5))
                        .color(palette.secondary),
                )
                .wrap()
                .selectable(false),
            );
        });
        return;
    }
    let mut actions = Vec::new();
    egui::ScrollArea::vertical()
        .id_salt("status-list")
        .auto_shrink([false, false])
        .show(&mut ui, |ui| {
            let mut section = None;
            for author in &authors {
                let heading = match (author.from_me, author.unseen) {
                    (true, _) => "My status",
                    (false, true) => "Recent updates",
                    (false, false) => "Viewed updates",
                };
                if section != Some(heading) {
                    section = Some(heading);
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        ui.add_space(16.0);
                        theme::text(
                            ui,
                            gettext(locale, heading),
                            theme::medium(13.0),
                            palette.accent,
                        );
                    });
                    ui.add_space(4.0);
                }
                author_row(app, ui, author, &seen, &mut actions);
            }
        });
    app.actions.extend(actions);
}

/// One author: their picture in a ring, green while something is unseen,
/// their name, and when they last posted.
fn author_row(
    app: &mut App,
    ui: &mut egui::Ui,
    author: &Author,
    seen: &HashSet<String>,
    actions: &mut Vec<Action>,
) {
    let palette = app.palette;
    let locale = app.locale;
    let name = if author.from_me {
        gettext(locale, "My status").into_owned()
    } else {
        app.display_name(author.id)
    };
    let picture = app.avatar(author.id);
    let (rect, _) = ui.allocate_exact_size(
        vec2(ui.available_width(), theme::ROW_HEIGHT),
        Sense::hover(),
    );
    let response = ui
        .interact(rect, row_id(author.id), Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    theme::reveal_focus(&response);
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), name.as_str())
    });
    let open = app
        .status_open
        .as_ref()
        .is_some_and(|(open, _)| open == author.id);
    if ui.is_rect_visible(rect) {
        if open {
            widgets::row_highlight(ui, &palette, rect, palette.surface_active);
        } else if response.hovered() || response.has_focus() {
            widgets::row_highlight(ui, &palette, rect, palette.surface_hover);
        }
        let center = pos2(rect.left() + 38.0, rect.center().y);
        let ring = if author.unseen {
            palette.accent
        } else {
            palette.dim
        };
        ui.painter()
            .circle_stroke(center, 25.0, egui::Stroke::new(2.0, ring));
        let avatar = Rect::from_center_size(center, Vec2::splat(42.0));
        widgets::paint_avatar(ui, &palette, avatar, &name, author.id, picture.as_deref());
        let left = rect.left() + 76.0;
        let width = (rect.right() - 12.0 - left).max(0.0);
        let font = if author.unseen {
            theme::semibold(14.5)
        } else {
            theme::medium(14.5)
        };
        widgets::line(ui, &name, font, palette.text, width, 1).paint(
            ui,
            pos2(left, rect.top() + 14.0),
            palette.text,
        );
        let when = crate::util::moment_stamp(locale, author.latest());
        widgets::line(ui, &when, theme::regular(13.0), palette.dim, width, 1).paint(
            ui,
            pos2(left, rect.top() + 38.0),
            palette.dim,
        );
    }
    if response.clicked() {
        actions.push(Action::ViewStatus(Some((
            author.id.to_owned(),
            author.first_unseen(seen).to_owned(),
        ))));
    }
}

/// The viewer's place while nothing is picked.
fn placeholder(app: &App, ui: &mut egui::Ui, rect: Rect) {
    let palette = app.palette;
    let mut ui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(Layout::top_down(Align::Center)),
    );
    ui.add_space((rect.height() / 2.0 - 60.0).max(24.0));
    theme::icon(&mut ui, Icon::Status, 56.0, palette.dim);
    ui.add_space(14.0);
    ui.set_max_width(360.0);
    ui.add(
        egui::Label::new(
            egui::RichText::new(gettext(
                app.locale,
                "Choose a contact to see their status updates.",
            ))
            .font(theme::regular(14.0))
            .color(palette.secondary),
        )
        .wrap()
        .selectable(false),
    );
}

/// Where the viewer goes from `id` by `step` statuses: on through the
/// author's statuses, then forward to the next author with something unseen
/// (seeing them reorders the list, so their place in it would skip people)
/// or back to the author above, and nowhere (the viewer closes) past them.
pub fn neighbour(
    statuses: &[Message],
    seen: &HashSet<String>,
    author: &str,
    id: &str,
    step: i32,
) -> Option<(String, String)> {
    let authors = authors(statuses, seen);
    let at = authors.iter().position(|each| each.id == author)?;
    let own = &authors[at].statuses;
    let index = own.iter().position(|status| status.id == id)? as i32 + step;
    if let Some(status) = usize::try_from(index).ok().and_then(|index| own.get(index)) {
        return Some((author.to_owned(), status.id.clone()));
    }
    if step > 0 {
        let other = authors
            .iter()
            .find(|other| other.id != author && other.unseen)?;
        return Some((other.id.to_owned(), other.first_unseen(seen).to_owned()));
    }
    let other = authors.get(at.checked_sub(1)?)?;
    Some((other.id.to_owned(), other.statuses.last()?.id.clone()))
}

/// One status, over the whole of `rect`.
fn viewer(app: &mut App, ui: &mut egui::Ui, rect: Rect, author: &str, id: &str) {
    let locale = app.locale;
    let ctx = ui.ctx().clone();
    let own: Vec<Message> = app
        .statuses
        .iter()
        .filter(|status| status.sender == author)
        .cloned()
        .collect();
    let Some(index) = own.iter().position(|status| status.id == id) else {
        return;
    };
    let status = &own[index];
    let mut actions = Vec::new();
    // Where the reader asked to go this frame; the first ask wins.
    let mut moved: Option<Option<(String, String)>> = None;
    ui.painter().rect_filled(rect, 0.0, BACKDROP);
    let room = super::caption_room(
        &ctx,
        Rect::from_min_size(rect.min, vec2(rect.width(), 64.0)),
        200.0,
    );
    let top = rect.top() + 10.0 + room.top;

    // Whether the status is ready to be timed: shown in full, not a video.
    let timed = match &status.content {
        Content::Image { media, .. } => media.path.is_some(),
        Content::Video { .. } => false,
        _ => true,
    };
    let started = egui::Id::new(("status-started", id));
    let now = ctx.input(|input| input.time);
    let elapsed = if timed {
        let start = ctx.data_mut(|data| *data.get_temp_mut_or_insert_with(started, || now));
        now - start
    } else {
        0.0
    };
    let progress = (elapsed / STATUS_SECONDS).clamp(0.0, 1.0) as f32;

    // A bar per status of the author: those before full, this one filling.
    let bars = Rect::from_min_max(
        pos2(rect.left() + 16.0 + room.left, top),
        pos2(rect.right() - 16.0 - room.right, top + 3.0),
    );
    let gap = 4.0;
    let count = own.len() as f32;
    let width = ((bars.width() - gap * (count - 1.0)) / count).max(1.0);
    for each in 0..own.len() {
        let x = bars.left() + each as f32 * (width + gap);
        let bar = Rect::from_min_size(pos2(x, bars.top()), vec2(width, bars.height()));
        ui.painter()
            .rect_filled(bar, 1.5, Color32::from_white_alpha(70));
        let fill = match each.cmp(&index) {
            std::cmp::Ordering::Less => 1.0,
            std::cmp::Ordering::Equal => progress,
            std::cmp::Ordering::Greater => 0.0,
        };
        if fill > 0.0 {
            let lit = Rect::from_min_size(bar.min, vec2(width * fill, bar.height()));
            ui.painter().rect_filled(lit, 1.5, Color32::WHITE);
        }
    }

    // The author, when they posted, and the way out.
    let header = Rect::from_min_max(
        pos2(bars.left(), bars.bottom() + 10.0),
        pos2(bars.right(), bars.bottom() + 50.0),
    );
    let name = if status.from_me {
        gettext(locale, "My status").into_owned()
    } else {
        app.display_name(author)
    };
    let picture = app.avatar(author);
    let avatar = Rect::from_min_size(header.min, Vec2::splat(36.0));
    widgets::paint_avatar(ui, &app.palette, avatar, &name, author, picture.as_deref());
    let text_left = avatar.right() + 10.0;
    let text_width = (header.right() - 48.0 - text_left).max(0.0);
    widgets::line(
        ui,
        &name,
        theme::semibold(14.5),
        Color32::WHITE,
        text_width,
        1,
    )
    .paint(ui, pos2(text_left, header.top() + 1.0), Color32::WHITE);
    let when = crate::util::moment_stamp(locale, status.timestamp);
    let gray = Color32::from_gray(190);
    widgets::line(ui, &when, theme::regular(12.5), gray, text_width, 1).paint(
        ui,
        pos2(text_left, header.top() + 20.0),
        gray,
    );
    let mut close = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(Rect::from_min_max(
                pos2(header.right() - 40.0, header.top()),
                header.max,
            ))
            .layout(Layout::right_to_left(Align::Center)),
    );
    if theme::icon_button(
        &mut close,
        Icon::X,
        20.0,
        gray,
        Color32::WHITE,
        &gettext(locale, "Close"),
    )
    .clicked()
    {
        moved.get_or_insert(None);
    }

    // The status itself, between the arrows that step through them.
    let caption = match &status.content {
        Content::Image { caption, .. } | Content::Video { caption, .. } => {
            caption.clone().filter(|caption| !caption.trim().is_empty())
        }
        _ => None,
    };
    let bottom = rect.bottom() - if caption.is_some() { 84.0 } else { 24.0 };
    let area = Rect::from_min_max(
        pos2(rect.left() + 64.0, header.bottom() + 12.0),
        pos2(rect.right() - 64.0, bottom),
    );
    if area.width() > 40.0 && area.height() > 40.0 {
        content(app, ui, area, status, &mut actions);
    }
    if let Some(caption) = caption {
        let mut below = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(Rect::from_min_max(
                    pos2(area.left(), bottom + 12.0),
                    pos2(area.right(), rect.bottom() - 12.0),
                ))
                .layout(Layout::top_down(Align::Center)),
        );
        below.add(
            egui::Label::new(
                egui::RichText::new(caption)
                    .font(theme::regular(15.0))
                    .color(Color32::WHITE),
            )
            .wrap(),
        );
    }
    let middle = (area.top() + area.bottom()) / 2.0;
    for (step, icon, label, x) in [
        (-1, Icon::ChevronLeft, "Previous status", rect.left() + 32.0),
        (1, Icon::ChevronRight, "Next status", rect.right() - 32.0),
    ] {
        let target = neighbour(&app.statuses, &app.statuses_seen, author, id, step);
        if target.is_none() {
            continue;
        }
        let mut arrow = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(Rect::from_center_size(pos2(x, middle), Vec2::splat(40.0)))
                .layout(Layout::centered_and_justified(egui::Direction::TopDown)),
        );
        if theme::circle_button(
            &mut arrow,
            icon,
            36.0,
            Color32::from_white_alpha(30),
            Color32::from_white_alpha(60),
            Color32::WHITE,
            &gettext(locale, label),
        )
        .clicked()
        {
            moved.get_or_insert(target);
        }
    }
    // The arrow keys step as the arrows do.
    let (back, on) = ctx.input(|input| {
        (
            input.key_pressed(egui::Key::ArrowLeft),
            input.key_pressed(egui::Key::ArrowRight),
        )
    });
    if back || on {
        let step = if on { 1 } else { -1 };
        moved.get_or_insert_with(|| neighbour(&app.statuses, &app.statuses_seen, author, id, step));
    }
    if timed && elapsed >= STATUS_SECONDS {
        moved.get_or_insert_with(|| neighbour(&app.statuses, &app.statuses_seen, author, id, 1));
    } else if timed {
        ctx.request_repaint();
    }
    if let Some(target) = moved {
        ctx.data_mut(|data| data.remove::<f64>(started));
        actions.push(Action::ViewStatus(target));
    }
    app.actions.extend(actions);
}

/// The picture, video or text of a status within `area`.
fn content(app: &App, ui: &mut egui::Ui, area: Rect, status: &Message, actions: &mut Vec<Action>) {
    let locale = app.locale;
    match &status.content {
        Content::Text { text, .. } | Content::Interactive { text, .. } => {
            let card = Rect::from_center_size(
                area.center(),
                vec2(area.width().min(560.0), area.height().min(560.0)),
            );
            ui.painter().rect_filled(card, 12.0, TEXT_CARD);
            let mut inside = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(card.shrink(28.0))
                    .layout(Layout::centered_and_justified(egui::Direction::TopDown)),
            );
            inside.add(
                egui::Label::new(
                    egui::RichText::new(text)
                        .font(theme::regular(24.0))
                        .color(Color32::WHITE),
                )
                .wrap()
                .halign(Align::Center),
            );
        }
        Content::Image { media, .. } | Content::Video { media, .. } => {
            let video = matches!(status.content, Content::Video { .. });
            let size = match (media.width, media.height) {
                (Some(width), Some(height)) if width > 0 && height > 0 => {
                    vec2(width as f32, height as f32)
                }
                _ => Vec2::splat(area.width().min(area.height())),
            };
            let scale = (area.width() / size.x).min(area.height() / size.y);
            let frame = Rect::from_center_size(area.center(), size * scale);
            let response = ui.interact(
                frame,
                egui::Id::new(("status-media", &status.id)),
                Sense::click(),
            );
            match (&media.path, video) {
                (Some(path), false) => {
                    egui::Image::new(crate::util::image_uri(path))
                        .corner_radius(8.0)
                        .paint_at(ui, frame);
                    if response.on_hover_cursor(egui::CursorIcon::ZoomIn).clicked() {
                        actions.push(Action::PreviewImage {
                            path: path.clone(),
                            chat: STATUS_CHAT.to_owned(),
                            message: status.id.clone(),
                            card: None,
                        });
                    }
                    return;
                }
                _ => {
                    if let Some(bytes) = status.thumbnail.as_deref().filter(|b| !b.is_empty()) {
                        let uri = super::conversation::thumbnail_uri(
                            ui.ctx(),
                            STATUS_CHAT,
                            &status.id,
                            bytes,
                        );
                        egui::Image::new(uri).corner_radius(8.0).paint_at(ui, frame);
                    } else {
                        ui.painter()
                            .rect_filled(frame, 8.0, Color32::from_white_alpha(12));
                    }
                }
            }
            let mut middle = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(Rect::from_center_size(
                        frame.center(),
                        vec2(frame.width(), 120.0),
                    ))
                    .layout(Layout::top_down(Align::Center)),
            );
            match (&media.path, &media.state) {
                (Some(path), _) => {
                    if theme::circle_button(
                        &mut middle,
                        Icon::Play,
                        64.0,
                        Color32::from_black_alpha(140),
                        Color32::from_black_alpha(180),
                        Color32::WHITE,
                        &gettext(locale, "Play video"),
                    )
                    .clicked()
                        || response.clicked()
                    {
                        actions.push(Action::ExpandVideo {
                            message: status.id.clone(),
                            path: path.clone(),
                        });
                    }
                }
                (None, MediaState::Failed(error)) => {
                    theme::text(
                        &mut middle,
                        error.as_str(),
                        theme::regular(13.5),
                        Color32::WHITE,
                    );
                    middle.add_space(6.0);
                    if theme::link(
                        &mut middle,
                        gettext(locale, "Try again").as_ref(),
                        theme::medium(13.5),
                        Color32::WHITE,
                    )
                    .clicked()
                    {
                        actions.push(download(status));
                    }
                }
                (None, MediaState::Downloading) => {
                    theme::spinner(&mut middle, 36.0, Color32::WHITE);
                }
                (None, MediaState::Idle) => {
                    // Opened is asked for: WhatsApp loads a status as it shows.
                    actions.push(download(status));
                    theme::spinner(&mut middle, 36.0, Color32::WHITE);
                }
            }
        }
        _ => {
            let mut middle = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(area)
                    .layout(Layout::centered_and_justified(egui::Direction::TopDown)),
            );
            middle.add(
                egui::Label::new(
                    egui::RichText::new(gettext(
                        locale,
                        "This status can't be shown here. See it on your phone.",
                    ))
                    .font(theme::regular(15.0))
                    .color(Color32::from_gray(200)),
                )
                .wrap(),
            );
        }
    }
}

fn download(status: &Message) -> Action {
    Action::Download {
        manual: true,
        card: None,
        chat: STATUS_CHAT.to_owned(),
        message: status.id.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Delivery;

    const ADA: &str = "4917000000001@s.whatsapp.net";
    const BOB: &str = "4917000000002@s.whatsapp.net";
    const ME: &str = "4917000000009@s.whatsapp.net";

    fn status(id: &str, sender: &str, timestamp: i64) -> Message {
        Message {
            id: id.to_owned(),
            chat: STATUS_CHAT.to_owned(),
            sender: sender.to_owned(),
            sender_name: None,
            from_me: sender == ME,
            timestamp,
            history_order: None,
            content: Content::Text {
                text: id.to_owned(),
                preview: None,
            },
            status: Delivery::None,
            delivered_at: None,
            read_at: None,
            quoted: None,
            reactions: Vec::new(),
            edited: false,
            mentions: Vec::new(),
            forwarded: false,
            thumbnail: None,
        }
    }

    fn statuses() -> Vec<Message> {
        vec![
            status("a1", ADA, 10),
            status("b1", BOB, 20),
            status("m1", ME, 25),
            status("a2", ADA, 30),
        ]
    }

    #[test]
    fn our_own_come_first_then_the_unseen_newest_first() {
        let statuses = statuses();
        let order = |seen: &HashSet<String>| {
            authors(&statuses, seen)
                .iter()
                .map(|author| (author.id, author.unseen))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            order(&HashSet::new()),
            [(ME, false), (ADA, true), (BOB, true)]
        );
        let seen = HashSet::from(["a1".to_owned(), "a2".to_owned()]);
        assert_eq!(order(&seen), [(ME, false), (BOB, true), (ADA, false)]);
    }

    #[test]
    fn a_row_opens_on_the_first_status_not_yet_seen() {
        let statuses = statuses();
        let seen = HashSet::from(["a1".to_owned()]);
        let authors = authors(&statuses, &seen);
        let ada = authors.iter().find(|author| author.id == ADA).unwrap();
        assert_eq!(ada.first_unseen(&seen), "a2");
        assert_eq!(ada.first_unseen(&HashSet::new()), "a1");
    }

    #[test]
    fn stepping_runs_through_an_author_then_on_to_the_next() {
        let statuses = statuses();
        let seen = HashSet::new();
        let step = |author, id, step| neighbour(&statuses, &seen, author, id, step);
        assert_eq!(step(ADA, "a1", 1), Some((ADA.into(), "a2".into())));
        assert_eq!(step(ADA, "a2", 1), Some((BOB.into(), "b1".into())));
        assert_eq!(step(BOB, "b1", -1), Some((ADA.into(), "a2".into())));
        assert_eq!(step(ME, "m1", -1), None);
        assert_eq!(step(BOB, "b1", 1), Some((ADA.into(), "a1".into())));
        let all = HashSet::from(["a1".into(), "a2".into(), "b1".into()]);
        assert_eq!(
            neighbour(&statuses, &all, BOB, "b1", 1),
            None,
            "with nothing left unseen, the viewer closes"
        );
    }
}
