//! Draws a WhatsApp Flow's screens in a dialog. The components follow the
//! Flow JSON reference; anything this does not draw is named as a part only
//! the phone shows, so the person knows the form is incomplete here.

use egui::{Align, CornerRadius, Frame, Layout, Margin};
use serde_json::{Value, json};

use crate::app::App;
use crate::flows::{FlowView, Request, walk};
use crate::model::Action;
use crate::theme::{self, Icon, Palette};

/// What a drawn component asked for this frame.
enum Asked {
    Act(Value),
}

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    let palette = app.palette;
    let locale = app.locale;
    let tr = |text: &'static str| crate::i18n::gettext(locale, text).to_string();
    let Some(mut flow) = app.flow.take() else {
        app.actions.push(Action::CloseDialog);
        return;
    };
    let title = flow
        .screen()
        .and_then(|screen| screen["title"].as_str())
        .map(|title| {
            flow.resolve(&json!(title))
                .as_str()
                .unwrap_or(title)
                .to_owned()
        })
        .unwrap_or_else(|| tr("Form"));
    ui.horizontal(|ui| {
        if flow.stack.len() > 1
            && !flow.sent
            && theme::icon_button(
                ui,
                Icon::ArrowLeft,
                16.0,
                palette.secondary,
                palette.text,
                &tr("Back"),
            )
            .clicked()
        {
            flow.back();
        }
        theme::text(ui, title, theme::bold(18.0), palette.text);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if theme::icon_button(
                ui,
                Icon::X,
                16.0,
                palette.secondary,
                palette.text,
                &tr("Close"),
            )
            .clicked()
            {
                app.actions.push(Action::CloseDialog);
            }
        });
    });
    ui.add_space(4.0);
    if flow.sent {
        theme::paragraph(
            ui,
            tr("Answer sent."),
            theme::regular(14.0),
            palette.secondary,
        );
        ui.add_space(8.0);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if theme::pill_button(ui, &palette, &tr("Close"), true).clicked() {
                app.actions.push(Action::CloseDialog);
            }
        });
        app.flow = Some(flow);
        return;
    }
    if let Some(error) = flow.error {
        theme::paragraph(
            ui,
            crate::i18n::gettext(locale, error),
            theme::regular(13.5),
            palette.danger,
        );
        ui.add_space(4.0);
    }
    let Some(screen) = flow.screen().cloned() else {
        if flow.busy {
            ui.horizontal(|ui| {
                theme::spinner(ui, 16.0, palette.accent);
                theme::text(
                    ui,
                    tr("Opening the form…"),
                    theme::regular(13.0),
                    palette.secondary,
                );
            });
        }
        app.flow = Some(flow);
        return;
    };
    if let Some(message) = flow.resolve(&json!("${data.error_message}")).as_str()
        && !message.is_empty()
    {
        theme::paragraph(ui, message, theme::regular(13.5), palette.danger);
        ui.add_space(4.0);
    }
    let mut asked = None;
    let mut footer = None;
    let height = (ui.ctx().content_rect().height() - 220.0).clamp(120.0, 520.0);
    egui::ScrollArea::vertical()
        .id_salt((
            "flow",
            &flow.message,
            &flow.current().map(|v| v.screen.clone()),
        ))
        .max_height(height)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            ui.add_enabled_ui(!flow.busy, |ui| {
                ui.spacing_mut().item_spacing.y = 10.0;
                let mut components = Vec::new();
                walk(
                    &screen["layout"]["children"],
                    &mut |c| components.push(c.clone()),
                    &flow,
                );
                for component in components {
                    if !flow.holds(component.get("visible").unwrap_or(&Value::Null)) {
                        continue;
                    }
                    if component["type"] == "Footer" {
                        footer = Some(component);
                        continue;
                    }
                    if let Some(Asked::Act(action)) =
                        draw(ui, &palette, locale, &mut flow, &component)
                    {
                        asked = Some(action);
                    }
                }
            });
        });
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        if flow.busy {
            theme::spinner(ui, 16.0, palette.accent);
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if let Some(footer) = &footer {
                let label = text_of(&flow, &footer["label"]);
                let ready = !flow.busy
                    && flow.complete_enough()
                    && footer["enabled"].as_bool() != Some(false);
                if ui
                    .add_enabled_ui(ready, |ui| theme::pill_button(ui, &palette, &label, true))
                    .inner
                    .clicked()
                {
                    asked = Some(footer["on-click-action"].clone());
                }
            }
        });
    });
    if let Some(action) = asked {
        let screen = flow.current().map(|v| v.screen.clone()).unwrap_or_default();
        match flow.act(&action) {
            Some(Request::Exchange(payload)) => app.actions.push(Action::FlowAction {
                screen,
                payload,
                complete: false,
            }),
            Some(Request::Complete(payload)) => app.actions.push(Action::FlowAction {
                screen,
                payload,
                complete: true,
            }),
            Some(Request::OpenUrl(url)) => app.actions.push(Action::OpenUrl(url)),
            None => {}
        }
    }
    app.flow = Some(flow);
}

fn text_of(flow: &FlowView, value: &Value) -> String {
    match flow.resolve(value) {
        Value::String(text) => text,
        Value::Array(lines) => lines
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// The options of a selection component, as (id, title, description).
fn options(flow: &FlowView, component: &Value) -> Vec<(String, String, String)> {
    flow.resolve(&component["data-source"])
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter(|item| item["enabled"].as_bool() != Some(false))
                .filter_map(|item| {
                    Some((
                        item["id"].as_str()?.to_owned(),
                        item["title"].as_str().unwrap_or_default().to_owned(),
                        item["description"].as_str().unwrap_or_default().to_owned(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn label(
    ui: &mut egui::Ui,
    palette: &Palette,
    locale: crate::i18n::Locale,
    flow: &FlowView,
    component: &Value,
) {
    let mut text = text_of(flow, &component["label"]);
    if text.is_empty() {
        return;
    }
    if component["required"].as_bool() != Some(true) {
        text.push(' ');
        text.push_str(&crate::i18n::gettext(locale, "(optional)"));
    }
    theme::paragraph(ui, text, theme::semibold(13.0), palette.secondary);
}

fn draw(
    ui: &mut egui::Ui,
    palette: &Palette,
    locale: crate::i18n::Locale,
    flow: &mut FlowView,
    component: &Value,
) -> Option<Asked> {
    let kind = component["type"].as_str().unwrap_or_default();
    let name = component["name"].as_str().unwrap_or_default().to_owned();
    let paragraph = |ui: &mut egui::Ui, flow: &FlowView, font, color| {
        let text = text_of(flow, &component["text"]);
        if !text.is_empty() {
            ui.add(
                egui::Label::new(egui::RichText::new(text).font(font).color(color))
                    .wrap()
                    .selectable(true),
            );
        }
    };
    match kind {
        "Form" | "If" | "Switch" => {}
        "TextHeading" => paragraph(ui, flow, theme::bold(17.0), palette.text),
        "TextSubheading" => paragraph(ui, flow, theme::semibold(15.0), palette.text),
        "TextBody" | "RichText" => paragraph(ui, flow, theme::regular(14.0), palette.text),
        "TextCaption" => paragraph(ui, flow, theme::regular(12.5), palette.secondary),
        "TextInput" | "TextArea" => {
            label(ui, palette, locale, flow, component);
            let visit = flow.stack.last_mut()?;
            let mut value = visit
                .form
                .get(&name)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let secret = matches!(
                component["input-type"].as_str(),
                Some("password" | "passcode")
            );
            let max = component["max-chars"]
                .as_u64()
                .unwrap_or(if kind == "TextArea" { 600 } else { 80 });
            let edit = if kind == "TextArea" {
                egui::TextEdit::multiline(&mut value)
            } else {
                egui::TextEdit::singleline(&mut value)
            };
            let response = Frame::new()
                .fill(palette.surface)
                .corner_radius(CornerRadius::same(theme::RADIUS))
                .inner_margin(Margin::symmetric(12, 8))
                .show(ui, |ui| {
                    ui.add(
                        edit.password(secret)
                            .char_limit(max as usize)
                            .font(theme::regular(15.0))
                            .text_color(palette.text)
                            .frame(egui::Frame::NONE)
                            .desired_width(f32::INFINITY),
                    )
                })
                .inner;
            if response.changed() {
                let value = match component["input-type"].as_str() {
                    Some("number" | "passcode") => {
                        value.chars().filter(char::is_ascii_digit).collect()
                    }
                    _ => value,
                };
                visit.form.insert(name, json!(value));
            }
            let helper = text_of(flow, &component["helper-text"]);
            if !helper.is_empty() {
                theme::paragraph(ui, helper, theme::regular(12.0), palette.dim);
            }
        }
        "Dropdown" => {
            label(ui, palette, locale, flow, component);
            let options = options(flow, component);
            let selected = flow
                .current()
                .and_then(|v| v.form.get(&name))
                .and_then(Value::as_str)
                .map(str::to_owned);
            let shown = options
                .iter()
                .find(|(id, ..)| Some(id) == selected.as_ref())
                .map(|(_, title, _)| title.clone())
                .unwrap_or_else(|| crate::i18n::gettext(locale, "Choose an option").to_string());
            let mut picked = None;
            egui::ComboBox::from_id_salt(("flow-dropdown", &name))
                .selected_text(shown)
                .width(ui.available_width())
                .show_ui(ui, |ui| {
                    for (id, title, description) in &options {
                        let text = if description.is_empty() {
                            title.clone()
                        } else {
                            format!("{title}\n{description}")
                        };
                        if ui
                            .selectable_label(selected.as_deref() == Some(id.as_str()), text)
                            .clicked()
                        {
                            picked = Some(id.clone());
                        }
                    }
                });
            if let Some(id) = picked {
                flow.stack.last_mut()?.form.insert(name, json!(id));
                return component.get("on-select-action").cloned().map(Asked::Act);
            }
        }
        "RadioButtonsGroup" | "ChipsSelector" | "CheckboxGroup" => {
            label(ui, palette, locale, flow, component);
            let many = kind != "RadioButtonsGroup";
            let options = options(flow, component);
            let visit = flow.stack.last_mut()?;
            let mut chosen: Vec<String> = match visit.form.get(&name) {
                Some(Value::Array(ids)) => ids
                    .iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect(),
                Some(Value::String(id)) => vec![id.clone()],
                _ => Vec::new(),
            };
            let mut changed = false;
            for (id, title, description) in &options {
                let mut on = chosen.contains(id);
                let text = if description.is_empty() {
                    title.clone()
                } else {
                    format!("{title} — {description}")
                };
                let clicked = if many {
                    ui.checkbox(&mut on, text).clicked()
                } else {
                    ui.radio(on, text).clicked() && {
                        on = true;
                        true
                    }
                };
                if clicked {
                    changed = true;
                    if !many {
                        chosen.clear();
                    }
                    chosen.retain(|c| c != id);
                    if on {
                        chosen.push(id.clone());
                    }
                }
            }
            if changed {
                let value = if many {
                    json!(chosen)
                } else {
                    json!(chosen.first())
                };
                visit.form.insert(name, value);
                return component.get("on-select-action").cloned().map(Asked::Act);
            }
        }
        "OptIn" => {
            let visit = flow.stack.last_mut()?;
            let mut on = visit
                .form
                .get(&name)
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let text = component["label"].as_str().unwrap_or_default().to_owned();
            if ui.checkbox(&mut on, text).changed() {
                visit.form.insert(name, json!(on));
            }
            if let Some(action) = component.get("on-click-action") {
                let more = crate::i18n::gettext(locale, "Read more").to_string();
                if theme::link(ui, more, theme::regular(13.0), palette.link).clicked() {
                    return Some(Asked::Act(action.clone()));
                }
            }
        }
        "EmbeddedLink" => {
            let text = text_of(flow, &component["text"]);
            if theme::link(ui, text, theme::regular(14.0), palette.link).clicked() {
                return component.get("on-click-action").cloned().map(Asked::Act);
            }
        }
        "Image" => {
            let alt = text_of(flow, &component["alt-text"]);
            if !alt.is_empty() {
                theme::paragraph(ui, format!("[{alt}]"), theme::regular(12.5), palette.dim);
            }
        }
        _ => {
            theme::paragraph(
                ui,
                crate::i18n::gettext(locale, "Part of this form only shows on your phone."),
                theme::regular(12.5),
                palette.dim,
            );
        }
    }
    None
}
