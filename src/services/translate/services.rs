//! Talking to the services: the body cut into pieces and requests, one
//! request shape per service, and their refusals in words.

use serde_json::{json, Value};

use crate::i18n::{i18n, i18n_f};

use super::detect::visible_text;
use super::{base, generation, translate, Service, Settings, BATCH, CHUNK, DONE};

/// The Reader View of a body as top-level blocks. Plain text, which the
/// reader shows line for line, is turned into paragraphs and line breaks
/// first: sent as markup, its newlines would otherwise be read as spaces.
pub(super) fn prepare_blocks(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    for block in crate::mail::reader::extract_blocks(body) {
        match block
            .strip_prefix("<div class=\"hylki-plain\">")
            .and_then(|b| b.strip_suffix("</div>"))
        {
            Some(text) => {
                for para in text
                    .split("\n\n")
                    .map(|p| p.trim_matches('\n'))
                    .filter(|p| !p.trim().is_empty())
                {
                    out.push(format!("<p>{}</p>", para.replace('\n', "<br>")));
                }
            }
            None if !block.trim().is_empty() => out.push(block),
            None => {}
        }
    }
    out
}

/// Blocks joined into pieces of at most [`CHUNK`] characters; a block
/// bigger than that on its own goes as one piece.
pub(super) fn chunk(blocks: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for b in blocks {
        match out.last_mut() {
            Some(last) if last.len() + b.len() <= CHUNK => last.push_str(b),
            _ => out.push(b.clone()),
        }
    }
    out
}

/// Pieces grouped into requests. LibreTranslate servers differ in whether
/// they take several texts at once, so it gets one at a time.
pub(super) fn batches(chunks: &[String], service: Service) -> Vec<Vec<String>> {
    let mut out: Vec<Vec<String>> = Vec::new();
    let mut size = 0;
    for c in chunks {
        match out.last_mut() {
            Some(last)
                if service != Service::Libre && size + c.len() <= BATCH && last.len() < 50 =>
            {
                size += c.len();
                last.push(c.clone());
            }
            _ => {
                size = c.len();
                out.push(vec![c.clone()]);
            }
        }
    }
    out
}

type Answer = (Vec<String>, Option<String>);

/// A request's result as JSON, with the service's own explanation when it
/// refused.
fn send(req: ureq::Request, body: Value) -> Result<Value, String> {
    match req.send_json(body) {
        Ok(resp) => resp
            .into_json::<Value>()
            .map_err(|_| i18n("The service’s answer could not be read.")),
        Err(ureq::Error::Status(code, resp)) => {
            let text = resp.into_string().unwrap_or_default();
            Err(refusal(code, &text))
        }
        Err(e) => Err(i18n_f(
            "The service could not be reached: {e}",
            &[("e", &e.to_string())],
        )),
    }
}

/// What a refusal means, in words.
pub(super) fn refusal(code: u16, body: &str) -> String {
    let said = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| {
            [
                v["message"].as_str(),
                v["error"]["message"].as_str(),
                v["error"].as_str(),
                v["detail"].as_str(),
            ]
            .into_iter()
            .flatten()
            .next()
            .map(str::to_string)
        })
        .unwrap_or_default();
    let what = match code {
        401 | 403 => i18n("The service did not accept the key."),
        429 => i18n("Too many requests: try again in a moment."),
        456 => i18n("This month’s allowance is used up."),
        _ => i18n_f(
            "The service refused the request (error {code}).",
            &[("code", &code.to_string())],
        ),
    };
    if said.is_empty() {
        what
    } else {
        format!("{what} ({said})")
    }
}

pub(super) fn deepl(
    agent: &ureq::Agent,
    key: &str,
    texts: &[String],
    to: &str,
) -> Result<Answer, String> {
    let host = if key.trim().ends_with(":fx") {
        "api-free.deepl.com"
    } else {
        "api.deepl.com"
    };
    let req = agent
        .post(&format!("https://{host}/v2/translate"))
        .set("Authorization", &format!("DeepL-Auth-Key {}", key.trim()));
    let v = send(
        req,
        json!({ "text": texts, "target_lang": to, "tag_handling": "html" }),
    )?;
    let list = v["translations"].as_array().cloned().unwrap_or_default();
    let detected = list
        .first()
        .and_then(|t| t["detected_source_language"].as_str())
        .map(str::to_string);
    Ok((
        list.iter()
            .map(|t| t["text"].as_str().unwrap_or_default().to_string())
            .collect(),
        detected,
    ))
}

pub(super) fn google(
    agent: &ureq::Agent,
    key: &str,
    texts: &[String],
    to: &str,
) -> Result<Answer, String> {
    let req = agent
        .post("https://translation.googleapis.com/language/translate/v2")
        .query("key", key.trim());
    let v = send(req, json!({ "q": texts, "target": to, "format": "html" }))?;
    let list = v["data"]["translations"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let detected = list
        .first()
        .and_then(|t| t["detectedSourceLanguage"].as_str())
        .map(str::to_string);
    Ok((
        list.iter()
            .map(|t| t["translatedText"].as_str().unwrap_or_default().to_string())
            .collect(),
        detected,
    ))
}

pub(super) fn microsoft(
    agent: &ureq::Agent,
    key: &str,
    region: &str,
    texts: &[String],
    to: &str,
) -> Result<Answer, String> {
    let mut req = agent
        .post("https://api.cognitive.microsofttranslator.com/translate")
        .query("api-version", "3.0")
        .query("to", to)
        .query("textType", "html")
        .set("Ocp-Apim-Subscription-Key", key.trim());
    if !region.trim().is_empty() {
        req = req.set("Ocp-Apim-Subscription-Region", region.trim());
    }
    let body: Vec<Value> = texts.iter().map(|t| json!({ "Text": t })).collect();
    let v = send(req, Value::Array(body))?;
    let list = v.as_array().cloned().unwrap_or_default();
    let detected = list
        .first()
        .and_then(|t| t["detectedLanguage"]["language"].as_str())
        .map(str::to_string);
    Ok((
        list.iter()
            .map(|t| {
                t["translations"][0]["text"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string()
            })
            .collect(),
        detected,
    ))
}

pub(super) fn libre(
    agent: &ureq::Agent,
    key: &str,
    url: &str,
    texts: &[String],
    to: &str,
) -> Result<Answer, String> {
    let base = url.trim().trim_end_matches('/');
    if !(base.starts_with("https://") || base.starts_with("http://")) {
        return Err(i18n(
            "Enter the LibreTranslate server’s address in Settings → Translation.",
        ));
    }
    let mut out = Vec::new();
    let mut detected = None;
    for text in texts {
        let mut body = json!({ "q": text, "source": "auto", "target": to, "format": "html" });
        if !key.trim().is_empty() {
            body["api_key"] = Value::String(key.trim().to_string());
        }
        let v = send(agent.post(&format!("{base}/translate")), body)?;
        if detected.is_none() {
            detected = v["detectedLanguage"]["language"]
                .as_str()
                .map(str::to_string);
        }
        out.push(v["translatedText"].as_str().unwrap_or_default().to_string());
    }
    Ok((out, detected))
}

/// Try the settings with a greeting, for the Settings page's check
/// (blocking): what was sent and what came back. The greeting is in a
/// language other than the target, or nothing would change.
pub(crate) fn check(settings: &Settings, key: &str) -> Result<(String, String), String> {
    let greeting = if base(&settings.target_language()) == "en" {
        "Guten Morgen"
    } else {
        "Good morning"
    };
    let probe = format!("{}\u{1f}check", generation());
    let t = translate(settings, key, &format!("<p>{greeting}</p>"), "", &probe)?;
    if let Ok(mut g) = DONE.lock() {
        if let Some(map) = g.as_mut() {
            map.remove(&probe);
        }
    }
    Ok((
        greeting.to_string(),
        visible_text(&t.html).trim().to_string(),
    ))
}
