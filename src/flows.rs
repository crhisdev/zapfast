//! The interface side of a WhatsApp Flow: which screen is showing, what the
//! person typed, and the `${…}` references a Flow JSON uses to read them.
//!
//! The worker holds the flow's secrets; this only holds what is on screen.

use serde_json::{Map, Value, json};

use crate::backend::FlowUpdate;
use crate::model::ChatId;

/// One screen the person has reached, with its data and form values.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Visit {
    pub screen: String,
    pub data: Value,
    pub form: Map<String, Value>,
}

/// An open flow.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FlowView {
    pub chat: ChatId,
    pub message: String,
    pub button: usize,
    /// The Flow JSON, once downloaded.
    pub json: Option<Value>,
    /// Screens visited, the one showing last.
    pub stack: Vec<Visit>,
    /// Waiting for WhatsApp or the business.
    pub busy: bool,
    pub error: Option<&'static str>,
    pub sent: bool,
}

/// What a component's action asks of the app.
#[derive(Clone, Debug, PartialEq)]
pub enum Request {
    /// Sends `payload` to the business from the screen showing.
    Exchange(Value),
    /// Finishes the flow, answering with `payload`.
    Complete(Value),
    OpenUrl(String),
}

impl FlowView {
    pub fn new(chat: ChatId, message: String, button: usize) -> Self {
        Self {
            chat,
            message,
            button,
            busy: true,
            ..Self::default()
        }
    }

    pub fn current(&self) -> Option<&Visit> {
        self.stack.last()
    }

    /// The JSON of the screen showing.
    pub fn screen(&self) -> Option<&Value> {
        let id = &self.current()?.screen;
        self.json.as_ref()?["screens"]
            .as_array()?
            .iter()
            .find(|screen| screen["id"].as_str() == Some(id))
    }

    pub fn apply(&mut self, update: FlowUpdate) {
        match update {
            FlowUpdate::Opened(json) => self.json = Some(json),
            FlowUpdate::Screen { screen, data } => {
                self.busy = false;
                self.error = None;
                self.show(screen, data);
            }
            FlowUpdate::Failed(error) => {
                self.busy = false;
                self.error = Some(error);
            }
            FlowUpdate::Sent => {
                self.busy = false;
                self.sent = true;
            }
        }
    }

    /// Shows `screen`: the same screen again keeps what was typed and takes
    /// the new data (an error message, a refreshed list); another one opens.
    fn show(&mut self, screen: String, data: Value) {
        let data = if data.is_object() { data } else { json!({}) };
        match self.stack.last_mut() {
            Some(visit) if visit.screen == screen => visit.data = data,
            _ => {
                let mut visit = Visit {
                    screen,
                    data,
                    form: Map::new(),
                };
                if let Some(json) = &self.json
                    && let Some(layout) = json["screens"]
                        .as_array()
                        .and_then(|s| s.iter().find(|s| s["id"].as_str() == Some(&visit.screen)))
                {
                    initial_values(&layout["layout"], &mut visit);
                }
                self.stack.push(visit);
            }
        }
    }

    /// Goes back a screen; false on the first one.
    pub fn back(&mut self) -> bool {
        if self.stack.len() > 1 && !self.busy {
            self.stack.pop();
            self.error = None;
            true
        } else {
            false
        }
    }

    /// Reads a value as the screen showing sees it, resolving `${…}`.
    pub fn resolve(&self, value: &Value) -> Value {
        match value {
            Value::String(text) => self.resolve_text(text),
            Value::Array(items) => Value::Array(items.iter().map(|v| self.resolve(v)).collect()),
            Value::Object(map) => Value::Object(
                map.iter()
                    .map(|(k, v)| (k.clone(), self.resolve(v)))
                    .collect(),
            ),
            other => other.clone(),
        }
    }

    fn resolve_text(&self, text: &str) -> Value {
        let trimmed = text.trim();
        if let Some(path) = trimmed
            .strip_prefix("${")
            .and_then(|rest| rest.strip_suffix('}'))
            .filter(|path| !path.contains("${"))
        {
            return self.lookup(path).unwrap_or(Value::Null);
        }
        if !text.contains("${") {
            return Value::String(text.to_owned());
        }
        // References inside text are replaced by their values as text.
        let mut out = String::new();
        let mut rest = text;
        while let Some(start) = rest.find("${") {
            out.push_str(&rest[..start]);
            let Some(end) = rest[start..].find('}') else {
                out.push_str(&rest[start..]);
                rest = "";
                break;
            };
            match self.lookup(&rest[start + 2..start + end]) {
                Some(Value::String(value)) => out.push_str(&value),
                Some(Value::Null) | None => {}
                Some(value) => out.push_str(&value.to_string()),
            }
            rest = &rest[start + end + 1..];
        }
        out.push_str(rest);
        Value::String(out)
    }

    /// `data.a.b`, `form.a`, or `screen.NAME.data.a` / `screen.NAME.form.a`.
    fn lookup(&self, path: &str) -> Option<Value> {
        let mut parts = path.trim().split('.');
        let (visit, scope) = match parts.next()? {
            "screen" => {
                let name = parts.next()?;
                let visit = self.stack.iter().rev().find(|v| v.screen == name)?;
                (visit, parts.next()?)
            }
            scope => (self.current()?, scope),
        };
        let mut value = match scope {
            "data" => &visit.data,
            "form" => {
                let field = parts.next()?;
                return visit.form.get(field).cloned().map(|value| {
                    parts.fold(value, |value, part| {
                        value.get(part).cloned().unwrap_or(Value::Null)
                    })
                });
            }
            _ => return None,
        };
        for part in parts {
            value = match value {
                Value::Array(items) => items.get(part.parse::<usize>().ok()?)?,
                _ => value.get(part)?,
            };
        }
        Some(value.clone())
    }

    /// Whether a `visible`/`If` condition holds on the screen showing.
    pub fn holds(&self, condition: &Value) -> bool {
        match condition {
            Value::Bool(value) => *value,
            Value::Null => true,
            Value::String(text) => self.condition(text),
            _ => truthy(&self.resolve(condition)),
        }
    }

    // shortcut: `&&`, `||`, `!`, `==`, `!=` without brackets or precedence
    // beyond `||` over `&&`; upgrade if a real flow needs more.
    fn condition(&self, text: &str) -> bool {
        let text = text.trim();
        if text.contains("||") {
            return text.split("||").any(|part| self.condition(part));
        }
        if text.contains("&&") {
            return text.split("&&").all(|part| self.condition(part));
        }
        let text = text.trim_matches(|c| c == '(' || c == ')').trim();
        for (operator, equal) in [("!=", false), ("==", true)] {
            if let Some((left, right)) = text.split_once(operator) {
                return (self.operand(left) == self.operand(right)) == equal;
            }
        }
        if let Some(rest) = text.strip_prefix('!') {
            return !self.condition(rest);
        }
        truthy(&self.operand(text))
    }

    fn operand(&self, text: &str) -> Value {
        let text = text.trim();
        if let Some(quoted) = text
            .strip_prefix('\'')
            .and_then(|t| t.strip_suffix('\''))
            .or_else(|| text.strip_prefix('"').and_then(|t| t.strip_suffix('"')))
        {
            return Value::String(quoted.to_owned());
        }
        if text.starts_with("${") {
            return self.resolve_text(text);
        }
        serde_json::from_str(text).unwrap_or_else(|_| Value::String(text.to_owned()))
    }

    /// Carries out a component's `on-click-action` / `on-select-action`.
    pub fn act(&mut self, action: &Value) -> Option<Request> {
        if self.busy {
            return None;
        }
        let payload = self.resolve(action.get("payload").unwrap_or(&json!({})));
        match action["name"].as_str()? {
            "navigate" => {
                let next = action["next"]["name"].as_str()?.to_owned();
                self.error = None;
                self.show_new(next, payload);
                None
            }
            "data_exchange" => {
                self.busy = true;
                self.error = None;
                Some(Request::Exchange(payload))
            }
            "complete" => {
                self.busy = true;
                self.error = None;
                Some(Request::Complete(payload))
            }
            "update_data" => {
                if let (Some(visit), Value::Object(changes)) = (self.stack.last_mut(), payload)
                    && let Value::Object(data) = &mut visit.data
                {
                    data.extend(changes);
                }
                None
            }
            "open_url" => action["url"]
                .as_str()
                .map(|url| Request::OpenUrl(url.to_owned())),
            _ => None,
        }
    }

    /// Navigating always opens the screen fresh, even the same one.
    fn show_new(&mut self, screen: String, data: Value) {
        let top = self.stack.last().map(|v| v.screen.clone());
        if top.as_deref() == Some(screen.as_str()) {
            self.stack.pop();
        }
        self.show(screen, data);
    }

    /// Whether every required field on the screen showing has a value.
    pub fn complete_enough(&self) -> bool {
        let (Some(screen), Some(visit)) = (self.screen(), self.current()) else {
            return false;
        };
        let mut missing = false;
        walk(
            &screen["layout"],
            &mut |component| {
                if component["required"].as_bool() == Some(true)
                    && self.holds(component.get("visible").unwrap_or(&Value::Null))
                    && let Some(name) = component["name"].as_str()
                    && !visit.form.get(name).is_some_and(filled)
                {
                    missing = true;
                }
            },
            self,
        );
        !missing
    }
}

fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::String(text) => !text.is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Number(n) => n.as_f64() != Some(0.0),
        Value::Object(_) => true,
    }
}

fn filled(value: &Value) -> bool {
    match value {
        Value::String(text) => !text.trim().is_empty(),
        Value::Bool(value) => *value,
        Value::Array(items) => !items.is_empty(),
        Value::Null => false,
        _ => true,
    }
}

/// Visits each component a screen shows, following `If` and `Switch`.
pub fn walk(node: &Value, visit: &mut dyn FnMut(&Value), view: &FlowView) {
    match node {
        Value::Array(items) => items.iter().for_each(|item| walk(item, visit, view)),
        Value::Object(_) => {
            match node["type"].as_str() {
                Some("If") => {
                    let branch = if view.holds(&node["condition"]) {
                        &node["then"]
                    } else {
                        &node["else"]
                    };
                    walk(branch, visit, view);
                    return;
                }
                Some("Switch") => {
                    let value = view.resolve(&node["value"]);
                    let key = value
                        .as_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| value.to_string());
                    walk(&node["cases"][key.as_str()], visit, view);
                    return;
                }
                _ => {}
            }
            visit(node);
            walk(&node["children"], visit, view);
        }
        _ => {}
    }
}

/// Fills a new screen's form with its components' initial values.
fn initial_values(layout: &Value, visit: &mut Visit) {
    let probe = FlowView {
        stack: vec![visit.clone()],
        ..FlowView::default()
    };
    let mut values = Map::new();
    let mut collect = |component: &Value| {
        if let Some(Value::Object(init)) = component.get("init-values").map(|v| probe.resolve(v)) {
            values.extend(init);
        }
        if let (Some(name), Some(init)) = (component["name"].as_str(), component.get("init-value"))
        {
            values.insert(name.to_owned(), probe.resolve(init));
        }
    };
    walk(layout, &mut collect, &probe);
    values.retain(|_, value| !value.is_null());
    visit.form = values;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flow() -> FlowView {
        let mut view = FlowView::new("chat".into(), "card".into(), 0);
        view.apply(FlowUpdate::Opened(json!({
            "version": "6.0",
            "screens": [
                {
                    "id": "PIX",
                    "title": "Confirmar Pix",
                    "layout": {"type": "SingleColumnLayout", "children": [
                        {"type": "TextBody", "text": "Valor: ${data.amount}"},
                        {"type": "Form", "name": "form", "children": [
                            {"type": "Dropdown", "name": "bank", "label": "Banco",
                             "required": true, "data-source": "${data.banks}"},
                            {"type": "If", "condition": "${form.bank} != ''", "then": [
                                {"type": "TextInput", "name": "pin", "label": "PIN",
                                 "input-type": "password", "required": true}
                            ]},
                            {"type": "Footer", "label": "Confirmar", "on-click-action": {
                                "name": "data_exchange",
                                "payload": {"bank": "${form.bank}", "pin": "${form.pin}"}
                            }}
                        ]}
                    ]}
                },
                {"id": "DONE", "title": "Pronto", "terminal": true,
                 "layout": {"type": "SingleColumnLayout", "children": [
                     {"type": "TextInput", "name": "note", "init-value": "${data.note}"}
                 ]}}
            ]
        })));
        view.apply(FlowUpdate::Screen {
            screen: "PIX".into(),
            data: json!({"amount": "R$ 41,50", "banks": [{"id": "nu", "title": "Nubank"}]}),
        });
        view
    }

    #[test]
    fn references_read_screen_data_and_the_form() {
        let mut view = flow();
        assert!(!view.busy);
        assert_eq!(
            view.resolve(&json!("Valor: ${data.amount}")),
            "Valor: R$ 41,50"
        );
        assert_eq!(view.resolve(&json!("${data.banks}"))[0]["title"], "Nubank");
        assert_eq!(view.resolve(&json!("${data.banks.0.id}")), "nu");
        assert_eq!(view.resolve(&json!("${form.bank}")), Value::Null);
        view.stack[0].form.insert("bank".into(), json!("nu"));
        assert_eq!(
            view.resolve(&json!({"b": "${form.bank}"})),
            json!({"b": "nu"})
        );
        assert_eq!(view.resolve(&json!("${screen.PIX.form.bank}")), "nu");
    }

    #[test]
    fn required_fields_follow_conditions() {
        let mut view = flow();
        assert!(!view.complete_enough());
        view.stack[0].form.insert("bank".into(), json!("nu"));
        // The PIN field appears once a bank is chosen, and is required.
        assert!(!view.complete_enough());
        view.stack[0].form.insert("pin".into(), json!("1234"));
        assert!(view.complete_enough());
    }

    #[test]
    fn actions_exchange_navigate_and_wait_for_the_business() {
        let mut view = flow();
        view.stack[0].form.insert("bank".into(), json!("nu"));
        view.stack[0].form.insert("pin".into(), json!("1234"));
        let footer = json!({"name": "data_exchange", "payload": {"bank": "${form.bank}", "pin": "${form.pin}"}});
        assert_eq!(
            view.act(&footer),
            Some(Request::Exchange(json!({"bank": "nu", "pin": "1234"})))
        );
        assert!(view.busy);
        assert_eq!(view.act(&footer), None, "one request at a time");
        view.apply(FlowUpdate::Screen {
            screen: "PIX".into(),
            data: json!({"error_message": "PIN incorreto"}),
        });
        assert_eq!(view.stack.len(), 1);
        assert_eq!(
            view.stack[0].form["pin"], "1234",
            "typed values survive an error"
        );
        assert_eq!(
            view.resolve(&json!("${data.error_message}")),
            "PIN incorreto"
        );
        view.act(&json!({"name": "navigate", "next": {"type": "screen", "name": "DONE"}, "payload": {"note": "ok"}}));
        assert_eq!(view.current().unwrap().screen, "DONE");
        assert_eq!(view.current().unwrap().form["note"], "ok");
        assert!(view.back());
        assert!(!view.back());
        assert_eq!(
            view.act(&json!({"name": "complete", "payload": {"done": true}})),
            Some(Request::Complete(json!({"done": true})))
        );
    }

    #[test]
    fn conditions() {
        let mut view = flow();
        view.stack[0].form.insert("bank".into(), json!("nu"));
        assert!(view.holds(&json!("${form.bank} == 'nu'")));
        assert!(view.holds(&json!("!${form.pin}")));
        assert!(view.holds(&json!("${form.bank} == 'x' || ${data.amount}")));
        assert!(!view.holds(&json!("${form.bank} && ${form.pin}")));
        assert!(view.holds(&json!(true)));
    }
}
