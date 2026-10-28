//! WhatsApp Flows: forms a business opens from a message button.
//!
//! Follows WhatsApp Web and the Flows runtime it embeds: the screens come
//! from `web.whatsapp.com/flows/json/<flow id>`; a flow that talks to the
//! business fetches the endpoint's key with a MEX query, checks it against
//! the business's identity key, and exchanges each screen's data sealed for
//! that key through Meta's GraphQL proxy. Finishing sends a native flow
//! response message, quoting the original, like any other interactive reply.
//! Secrets (flow token, keys, proxy secret) never leave the worker.

use std::sync::Arc;

use serde_json::{Value, json};
use whatsapp_rust::Client;

use super::{ChatId, Command, Event, Jid, MessageExt, Worker, wa};
use crate::backend::FlowUpdate;
use whatsapp_rust::waproto::buffa::Message as _;

mod crypto;

const METADATA_DOC: whatsapp_rust::MexDoc = whatsapp_rust::MexDoc {
    name: "WAWebGalaxyFlowsDrawerGetFlowDataQuery",
    id: "32671434985780838",
};
const FLOW_JSON_URL: &str = "https://web.whatsapp.com/flows/json/";
const ENDPOINT_URL: &str = "https://graph.whatsapp.com/graphql/extensions";
const ENDPOINT_DOC: &str = "9793760614036679";
/// The Flows runtime's public client token, sent with every endpoint request.
const ENDPOINT_TOKEN: &str = "WA|471011608249857|4b543e9203c0b420cb5617b71ff0b80a";
const LONGEST_REPLY: u64 = 4 * 1024 * 1024;

/// What a `galaxy_message` button carries.
#[derive(Clone, Debug, PartialEq)]
pub struct Params {
    flow_id: String,
    flow_token: String,
    label: String,
    exchange: bool,
    screen: Option<String>,
    data: Value,
    data_api_version: Option<String>,
    json: Option<Value>,
    name: Option<String>,
}

/// Reads a flow button's parameters; `None` for anything that is not one.
pub(super) fn params(value: &Value) -> Option<Params> {
    let text = |key: &str| {
        value
            .get(key)
            .and_then(Value::as_str)
            .filter(|v| !v.trim().is_empty())
            .map(str::to_owned)
    };
    let payload = value.get("flow_action_payload");
    Some(Params {
        flow_id: text("flow_id")?,
        flow_token: text("flow_token")?,
        label: text("flow_cta")?,
        exchange: text("flow_action").as_deref() == Some("data_exchange"),
        screen: payload
            .and_then(|p| p.get("screen"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        data: payload
            .and_then(|p| p.get("data"))
            .cloned()
            .unwrap_or_else(|| json!({})),
        data_api_version: value
            .get("flow_metadata")
            .and_then(|m| m.get("data_api_version"))
            .and_then(|v| {
                v.as_str()
                    .map(str::to_owned)
                    .or_else(|| v.as_i64().map(|v| v.to_string()))
            }),
        json: value.get("flow_json").and_then(|v| match v {
            Value::String(text) => serde_json::from_str(text).ok(),
            Value::Object(_) => Some(v.clone()),
            _ => None,
        }),
        name: text("flow_name"),
    })
}

/// An open flow's secrets, kept by the worker between screens.
#[derive(Clone, Debug)]
pub struct Session {
    params: Params,
    key: Option<String>,
    proxy_secret: Option<String>,
    token_signature: Option<String>,
    version: String,
}

/// Where a request to the business left the flow.
#[derive(Debug, PartialEq)]
pub enum Step {
    Screen { screen: String, data: Value },
    Complete(Value),
}

fn get(agent: &ureq::Agent, url: &str) -> Result<Value, &'static str> {
    let mut response = agent.get(url).call().map_err(|e| {
        log::warn!("flow: {e}");
        "could not download the form"
    })?;
    let body = response
        .body_mut()
        .with_config()
        .limit(LONGEST_REPLY)
        .read_to_string()
        .map_err(|e| {
            log::warn!("flow: {e}");
            "could not download the form"
        })?;
    serde_json::from_str(&body).map_err(|_| "the form WhatsApp sent is not valid")
}

/// The endpoint key's metadata; `None` fields when the flow has no endpoint.
async fn metadata(client: &Client, biz: &str, flow_id: &str) -> Result<Value, &'static str> {
    let request = whatsapp_rust::MexRequest::new_raw(
        METADATA_DOC,
        &["request"],
        json!({"request": {"extensions": {"biz_jid": biz, "flow_id": flow_id}}}),
    );
    let response = client.mex().execute(request).await.map_err(|e| {
        log::warn!("flow: {e}");
        "WhatsApp did not describe this form"
    })?;
    response
        .data
        .and_then(|mut data| {
            data.get_mut("xwa_extensions_get_flow_data")
                .map(Value::take)
        })
        .ok_or("WhatsApp did not describe this form")
}

/// Checks the endpoint key's signature with the business's identity key, as
/// WhatsApp Web does before it trusts the key with anything.
async fn verify(client: &Client, identities: &[Jid], key: &str, signature: &str) -> bool {
    use base64::Engine as _;
    use whatsapp_rust::wacore::libsignal::core::curve::PublicKey;
    use whatsapp_rust::wacore::types::jid::JidExt as _;
    let b64 = base64::engine::general_purpose::STANDARD;
    let (Ok(message), Ok(signature)) = (b64.decode(key), b64.decode(signature)) else {
        return false;
    };
    let backend = client.persistence_manager().backend();
    for jid in identities {
        let address = jid.to_protocol_address();
        if let Ok(Some(identity)) = backend.load_identity(address.as_str()).await
            && let Ok(identity) = PublicKey::from_djb_public_key_bytes(&identity)
        {
            return identity.verify_signature(&message, &signature);
        }
    }
    false
}

/// Opens a flow: its screens, the endpoint key when it has an endpoint, and
/// the first screen with its data.
pub(super) async fn open(
    client: Arc<Client>,
    biz: String,
    identities: Vec<Jid>,
    params: Params,
) -> Result<(Session, Value, Step), &'static str> {
    let json = match params.json.clone() {
        Some(json) => json,
        None => {
            let url = format!("{FLOW_JSON_URL}{}", params.flow_id);
            let agent = crate::proxy::agent();
            tokio::task::spawn_blocking(move || get(&agent, &url))
                .await
                .map_err(|_| "could not download the form")??
        }
    };
    if json
        .get("screens")
        .and_then(Value::as_array)
        .is_none_or(Vec::is_empty)
    {
        return Err("the form WhatsApp sent has no screens");
    }
    let version = params.data_api_version.clone().or_else(|| {
        json.get("data_api_version")
            .and_then(Value::as_str)
            .map(str::to_owned)
    });
    let mut session = Session {
        params,
        key: None,
        proxy_secret: None,
        token_signature: None,
        version: version.clone().unwrap_or_else(|| "3.0".into()),
    };
    if session.params.exchange || version.is_some() {
        let data = metadata(&client, &biz, &session.params.flow_id).await?;
        let key = data["endpoint_public_key"]["key"]
            .as_str()
            .unwrap_or_default();
        let signature = data["endpoint_public_key"]["signature"]
            .as_str()
            .unwrap_or_default();
        if key.is_empty() || !verify(&client, &identities, key, signature).await {
            return Err("this business's encryption key could not be confirmed");
        }
        let flow = data["extensions_flow_data"]
            .as_array()
            .and_then(|flows| {
                flows
                    .iter()
                    .find(|flow| flow["flow_id"].as_str() == Some(&session.params.flow_id))
            })
            .map(|flow| &flow["extensions"]["metadata"]);
        let field = |name: &str| {
            flow.and_then(|m| m[name].as_str())
                .filter(|v| !v.is_empty())
                .map(str::to_owned)
        };
        session.key = Some(key.to_owned());
        session.proxy_secret = field("www_proxy_secret");
        session.token_signature = field("flow_token_signature");
    }
    let step = if session.params.exchange {
        exchange(&session, "INIT", "", json!({})).await?
    } else {
        Step::Screen {
            screen: session.params.screen.clone().unwrap_or_else(|| {
                json["screens"][0]["id"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned()
            }),
            data: session.params.data.clone(),
        }
    };
    Ok((session, json, step))
}

/// The JSON the business's endpoint receives, before sealing.
fn request(session: &Session, action: &str, screen: &str, data: Value) -> Value {
    let mut request = json!({
        "data": data,
        "flow_token": session.params.flow_token,
        "screen": screen,
        "action": action,
        "version": session.version,
    });
    if session.version == "4.0"
        && let Some(signature) = &session.token_signature
    {
        request["flow_token_signature"] = json!(signature);
    }
    request
}

/// Reads the endpoint's decrypted answer.
fn step(answer: &str) -> Result<Step, &'static str> {
    let answer: Value =
        serde_json::from_str(answer).map_err(|_| "the business's answer is not valid")?;
    let screen = answer["screen"]
        .as_str()
        .ok_or("the business's answer has no screen")?
        .to_owned();
    let data = answer.get("data").cloned().unwrap_or_else(|| json!({}));
    if screen == "SUCCESS" {
        return Ok(Step::Complete(
            data["extension_message_response"]["params"].clone(),
        ));
    }
    Ok(Step::Screen { screen, data })
}

/// Sends one data exchange to the business and waits for its next screen.
pub(super) async fn exchange(
    session: &Session,
    action: &str,
    screen: &str,
    data: Value,
) -> Result<Step, &'static str> {
    let key = session
        .key
        .as_deref()
        .ok_or("this form cannot talk to the business")?;
    let sealed = crypto::seal(key, &request(session, action, screen, data).to_string())?;
    let body = json!({
        "access_token": ENDPOINT_TOKEN,
        "doc_id": ENDPOINT_DOC,
        "variables": {
            "encrypted_flow_data": sealed.data,
            "encrypted_aes_key": sealed.key,
            "initial_vector": sealed.iv,
            "flow_or_version_id": session.params.flow_id,
            "www_proxy_signature": session.proxy_secret,
        },
    })
    .to_string();
    let agent = crate::proxy::agent();
    let reply = tokio::task::spawn_blocking(move || {
        let mut response = agent
            .post(ENDPOINT_URL)
            .header("Content-Type", "application/json")
            .send(body)
            .map_err(|e| {
                log::warn!("flow: {e}");
                "could not reach the business"
            })?;
        response
            .body_mut()
            .with_config()
            .limit(LONGEST_REPLY)
            .read_to_string()
            .map_err(|e| {
                log::warn!("flow: {e}");
                "could not reach the business"
            })
    })
    .await
    .map_err(|_| "could not reach the business")??;
    let reply: Value =
        serde_json::from_str(&reply).map_err(|_| "the business's answer is not valid")?;
    let result = &reply["data"]["xwa_flows_make_flow_endpoint_request"];
    match result["status_code"].as_i64() {
        Some(200) => {}
        Some(427) => return Err("this form has expired"),
        Some(504) => return Err("the business took too long to answer"),
        Some(code) => {
            log::warn!("flow: endpoint status {code}");
            return Err("the business could not answer");
        }
        None => return Err("the business could not answer"),
    }
    step(&sealed.open(
        result["body"].as_str().unwrap_or_default(),
        &session.version,
    )?)
}

/// The response message WhatsApp sends when a flow finishes.
fn response(session: &Session, mut params: Value) -> wa::Message {
    use wa::message::interactive_response_message::{
        Body, InteractiveResponseMessage, NativeFlowResponseMessage, body::Format,
    };
    if !params.is_object() {
        params = json!({});
    }
    if let Some(message) = params
        .as_object_mut()
        .and_then(|p| p.remove("response_message"))
    {
        params["wa_flow_response_params"] = json!({
            "response_message": message,
            "flow_name": session.params.name,
            "flow_id": session.params.flow_id,
            "title": session.params.label,
        });
    }
    params["flow_token"] = json!(session.params.flow_token);
    params["flow_message_version"] = json!(1);
    let mut message = wa::Message::default();
    message.interactive_response_message = super::MessageField::some({
        let mut proto = wa::message::InteractiveResponseMessage::default();
        proto.body = super::MessageField::some({
            let mut body = Body::default();
            body.text = Some("Sent".into());
            body.format = Some(Format::EXTENSIONS_1);
            body
        });
        proto.interactive_response_message = Some(
            InteractiveResponseMessage::NativeFlowResponseMessage(Box::new({
                let mut flow = NativeFlowResponseMessage::default();
                flow.name = Some("galaxy_message".into());
                flow.params_json = Some(params.to_string());
                flow.version = Some(1);
                flow
            })),
        );
        proto
    });
    message
}

impl Worker {
    /// The business's identity, by phone number and by privacy id.
    fn identities(&self, chat: &str) -> Vec<Jid> {
        let mut jids: Vec<Jid> = Self::jid_of(chat).into_iter().collect();
        if let Some(jid) = jids.first()
            && let Some((lid, _)) = self
                .lid_to_pn
                .iter()
                .find(|(_, pn)| **pn == jid.user_base())
            && let Ok(lid) = format!("{lid}@lid").parse()
        {
            jids.push(lid);
        }
        jids
    }

    pub(in super::super) fn open_flow(&mut self, chat: ChatId, message: String, button: usize) {
        let flow = (|| {
            let raw = self.archive.raw(&chat, &message).ok().flatten()?;
            let original = wa::Message::decode_from_slice(&raw).ok()?;
            super::interactive::flow(original.get_base_message(), button)
        })();
        let (Some(client), Some(params)) = (self.client.clone(), flow) else {
            self.emit_flow(
                chat,
                message,
                FlowUpdate::Failed("This form is unavailable"),
            );
            return;
        };
        self.flows.remove(&(chat.clone(), message.clone()));
        let identities = self.identities(&chat);
        let commands = self.commands.clone();
        let biz = chat.clone();
        tokio::spawn(async move {
            let result = open(client, biz, identities, params).await;
            let _ = commands.send(Command::FlowOpened {
                chat,
                message,
                result: result.map(Box::new),
            });
        });
    }

    pub(in super::super) fn flow_opened(
        &mut self,
        chat: ChatId,
        message: String,
        result: Result<Box<(Session, Value, Step)>, &'static str>,
    ) {
        match result.map(|opened| *opened) {
            Ok((session, json, step)) => {
                self.flows.insert((chat.clone(), message.clone()), session);
                self.emit_flow(chat.clone(), message.clone(), FlowUpdate::Opened(json));
                self.flow_stepped(chat, message, Ok(step));
            }
            Err(error) => self.emit_flow(chat, message, FlowUpdate::Failed(error)),
        }
    }

    /// A screen's action: exchanges data with the business, or with
    /// `complete` finishes the flow with these response parameters.
    pub(in super::super) fn flow_action(
        &mut self,
        chat: ChatId,
        message: String,
        screen: String,
        payload: Value,
        complete: bool,
    ) {
        let Some(session) = self.flows.get(&(chat.clone(), message.clone())).cloned() else {
            self.emit_flow(
                chat,
                message,
                FlowUpdate::Failed("This form is no longer open"),
            );
            return;
        };
        if complete {
            self.flow_stepped(chat, message, Ok(Step::Complete(payload)));
            return;
        }
        let commands = self.commands.clone();
        tokio::spawn(async move {
            let result = exchange(&session, "data_exchange", &screen, payload).await;
            let _ = commands.send(Command::FlowStepped {
                chat,
                message,
                result,
            });
        });
    }

    pub(in super::super) fn flow_stepped(
        &mut self,
        chat: ChatId,
        message: String,
        result: Result<Step, &'static str>,
    ) {
        let key = (chat.clone(), message.clone());
        match result {
            Ok(Step::Screen { screen, data }) => {
                self.emit_flow(chat, message, FlowUpdate::Screen { screen, data })
            }
            Ok(Step::Complete(params)) => {
                let Some(session) = self.flows.remove(&key) else {
                    return;
                };
                let reply = response(&session, params);
                if self.send_interactive_reply(chat.clone(), message.clone(), reply) {
                    self.emit_flow(chat, message, FlowUpdate::Sent);
                } else {
                    self.emit_flow(
                        chat,
                        message,
                        FlowUpdate::Failed("The answer could not be sent"),
                    );
                }
            }
            Err(error) => self.emit_flow(chat, message, FlowUpdate::Failed(error)),
        }
    }

    pub(in super::super) fn close_flow(&mut self, chat: ChatId, message: String) {
        self.flows.remove(&(chat, message));
    }

    fn emit_flow(&self, chat: ChatId, message: String, update: FlowUpdate) {
        self.emit(Event::Flow {
            chat,
            message,
            update,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(version: &str) -> Session {
        Session {
            params: params(&json!({
                "flow_id": "123",
                "flow_token": "token",
                "flow_cta": "Confirmar",
                "flow_action": "data_exchange",
                "flow_name": "pix",
            }))
            .unwrap(),
            key: None,
            proxy_secret: None,
            token_signature: Some("signed".into()),
            version: version.into(),
        }
    }

    #[test]
    fn button_parameters_need_an_id_a_token_and_a_label() {
        let value = json!({
            "flow_message_version": "3",
            "flow_token": "token",
            "flow_id": "123",
            "flow_cta": "Confirmar",
            "flow_action": "navigate",
            "flow_action_payload": {"screen": "START", "data": {"amount": "41,50"}},
            "flow_metadata": {"data_api_version": "3.0"},
        });
        let parsed = params(&value).unwrap();
        assert!(!parsed.exchange);
        assert_eq!(parsed.screen.as_deref(), Some("START"));
        assert_eq!(parsed.data["amount"], "41,50");
        assert_eq!(parsed.data_api_version.as_deref(), Some("3.0"));
        for key in ["flow_id", "flow_token", "flow_cta"] {
            let mut value = value.clone();
            value.as_object_mut().unwrap().remove(key);
            assert!(params(&value).is_none(), "{key}");
        }
    }

    #[test]
    fn requests_carry_the_token_and_sign_it_only_from_version_four() {
        let three = request(&session("3.0"), "INIT", "", json!({}));
        assert_eq!(three["flow_token"], "token");
        assert_eq!(three["action"], "INIT");
        assert!(three.get("flow_token_signature").is_none());
        let four = request(
            &session("4.0"),
            "data_exchange",
            "PIN",
            json!({"pin": "1234"}),
        );
        assert_eq!(four["flow_token_signature"], "signed");
        assert_eq!(four["data"]["pin"], "1234");
    }

    #[test]
    fn success_finishes_with_the_business_parameters() {
        assert_eq!(
            step(
                r#"{"screen":"SUCCESS","data":{"extension_message_response":{"params":{"ok":true}}}}"#
            ),
            Ok(Step::Complete(json!({"ok": true})))
        );
        assert_eq!(
            step(r#"{"screen":"PIN","data":{"error_message":"PIN incorreto"}}"#),
            Ok(Step::Screen {
                screen: "PIN".into(),
                data: json!({"error_message": "PIN incorreto"})
            })
        );
        assert!(step(r#"{"data":{}}"#).is_err());
    }

    #[test]
    fn the_response_message_is_the_one_whatsapp_sends() {
        let message = response(
            &session("3.0"),
            json!({"bank": "nu", "response_message": "{}"}),
        );
        let reply = message.interactive_response_message.as_option().unwrap();
        let Some(wa::message::interactive_response_message::InteractiveResponseMessage::NativeFlowResponseMessage(flow)) =
            &reply.interactive_response_message
        else {
            panic!("native flow response")
        };
        assert_eq!(flow.name.as_deref(), Some("galaxy_message"));
        assert_eq!(flow.version, Some(1));
        let params: Value = serde_json::from_str(flow.params_json.as_deref().unwrap()).unwrap();
        assert_eq!(params["bank"], "nu");
        assert_eq!(params["flow_token"], "token");
        assert_eq!(params["flow_message_version"], 1);
        assert!(params.get("response_message").is_none());
        assert_eq!(params["wa_flow_response_params"]["flow_id"], "123");
        assert_eq!(params["wa_flow_response_params"]["title"], "Confirmar");
    }
}
