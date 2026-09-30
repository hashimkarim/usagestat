//! Shared catalogue choices resolve only bundled/registered SVG assets.
use crate::{response_json, response_text};
use serde_json::{Value, json};
use std::path::Path;
use std::sync::OnceLock;
use usagestat_core::ProviderSummary;

fn catalog() -> &'static Value {
    static CATALOG: OnceLock<Value> = OnceLock::new();
    CATALOG.get_or_init(|| {
        let source = include_str!("../../../plugins/_provider-icons/manifest.js");
        let json = source
            .split_once("export const catalog = ")
            .expect("pinned icon catalogue")
            .1
            .trim()
            .trim_end_matches(';');
        serde_json::from_str(json).expect("pinned icon catalogue JSON")
    })
}

pub fn public_catalog() -> Value {
    let source = catalog();
    let icons: Vec<_> = source["icons"]
        .as_object()
        .expect("icon entries")
        .iter()
        .map(|(id, value)| {
            json!({
                "id":id,"name":value["name"],"alternatives":value["alternatives"],
                "monochrome":true,"color":value.get("color").is_some(),
            })
        })
        .collect();
    json!({"icons":icons,"aliases":source["aliases"]})
}

fn parameter<'a>(query: &'a str, key: &str) -> Option<&'a str> {
    query
        .split('&')
        .filter_map(|part| part.split_once('='))
        .find(|(k, _)| *k == key)
        .map(|(_, value)| value)
}

fn read_svg(path: &Path) -> Option<String> {
    if std::fs::metadata(path).ok()?.len() > 128 * 1024 {
        return None;
    }
    std::fs::read_to_string(path)
        .ok()
        .filter(|s| s.len() <= 128 * 1024)
}

pub fn serve(id: &str, query: &str, providers: &[ProviderSummary]) -> Option<String> {
    let style = parameter(query, "style");
    let source = parameter(query, "source");
    if style.is_none() && source.is_none() {
        return None;
    }
    let style = style.unwrap_or("color");
    if !["monochrome", "color"].contains(&style) {
        return Some(response_json(
            400,
            "Bad Request",
            r#"{"error":"invalid_icon_style"}"#,
        ));
    }
    let provider = providers.iter().find(|p| p.id == id);
    let chosen =
        source.unwrap_or_else(|| provider.and_then(|p| p.plugin_id.as_deref()).unwrap_or(id));
    let chosen = catalog()["aliases"][chosen].as_str().unwrap_or(chosen);
    let entry = &catalog()["icons"][chosen];
    if let Some(file) = entry
        .get(style)
        .or_else(|| entry.get("monochrome"))
        .and_then(Value::as_str)
    {
        if Path::new(file).components().count() != 1 || !file.ends_with(".svg") {
            return None;
        }
        for provider in providers {
            if let Some(icon) = &provider.icon {
                for path in [&icon.path, &icon.monochrome_path, &icon.color_path]
                    .into_iter()
                    .flatten()
                {
                    let Some(parent) = Path::new(path).parent() else {
                        continue;
                    };
                    let root = if parent.file_name().is_some_and(|n| n == "_provider-icons") {
                        parent.to_path_buf()
                    } else {
                        parent.parent().unwrap_or(parent).join("_provider-icons")
                    };
                    if let Some(svg) = read_svg(&root.join(file)) {
                        return Some(response_text(200, "OK", "image/svg+xml", &svg));
                    }
                }
            }
        }
    }
    // Providers outside the shared catalogue can still choose the monochrome
    // and colour variants declared by their own plugin manifest.
    if source.is_none() {
        if let Some(icon) = provider.and_then(|p| p.icon.as_ref()) {
            let file = if style == "monochrome" {
                icon.monochrome_path.as_ref().or(icon.path.as_ref())
            } else {
                icon.color_path.as_ref().or(icon.path.as_ref())
            };
            if let Some(svg) = file.and_then(|file| read_svg(Path::new(file))) {
                return Some(response_text(200, "OK", "image/svg+xml", &svg));
            }
        }
    }
    Some(response_json(
        404,
        "Not Found",
        r#"{"error":"icon_not_found"}"#,
    ))
}
