use leptos::prelude::*;
use serde_json::Value;

#[component]
pub fn Icon(#[prop(into)] name: String) -> impl IntoView {
    let path = match name.as_str() {
        "plus" => "M12 5v14m-7-7h14",
        "search" => "m21 21-4.6-4.6M19 10.5a8.5 8.5 0 1 1-17 0 8.5 8.5 0 0 1 17 0",
        "channel" => "M9 3 7 21m10-18-2 18M3 9h18M2 15h18",
        "arrow" => "m9 5 7 7-7 7",
        "back" => "m15 5-7 7 7 7",
        "close" => "m6 6 12 12M6 18 18 6",
        "attach" => {
            "m21 11-9 9a6 6 0 0 1-8.5-8.5l10-10a4 4 0 0 1 5.7 5.7L9 17a2 2 0 0 1-2.8-2.8l9-9"
        }
        "send" => "m22 2-7 20-4-9-9-4 20-7ZM22 2 11 13",
        "settings" => {
            "M12 8a4 4 0 1 0 0 8 4 4 0 0 0 0-8Zm-9 4 2-2-1-3 3-3 3 1 2-2 2 2 3-1 3 3-1 3 2 2-2 2 1 3-3 3-3-1-2 2-2-2-3 1-3-3 1-3-2-2Z"
        }
        "check" => "m5 12 4 4L19 6",
        "clock" => "M12 8v5l3 2M22 12a10 10 0 1 1-20 0 10 10 0 0 1 20 0",
        "refresh" => {
            "M20 7v5h-5M4 17v-5h5M5.6 7a8 8 0 0 1 13.2-1L20 8M4 16l1.2 2A8 8 0 0 0 18.4 17"
        }
        "menu" => "M4 6h16M4 12h16M4 18h16",
        "edit" => "m16 3 5 5-13 13H3v-5L16 3Zm-2 2 5 5",
        "link" => {
            "m10 13 4-4m-6 7H6a4 4 0 0 1-3-7l4-4a4 4 0 0 1 6 0m-2 14a4 4 0 0 0 6 0l4-4a4 4 0 0 0-3-7h-2"
        }
        _ => "M5 12h14",
    };
    view! {
        <svg
            width="18"
            height="18"
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            stroke-width="1.65"
            stroke-linecap="round"
            stroke-linejoin="round"
            aria-hidden="true"
        >
            <path d=path />
        </svg>
    }
}

#[component]
pub fn StateBadge(#[prop(into)] state: String) -> impl IntoView {
    let label = state.replace('_', " ");
    view! { <span class=format!("state-badge state-{state}")>{label}</span> }
}

#[component]
pub fn Markdown(#[prop(into)] text: String) -> impl IntoView {
    let html = crate::content::markdown(&text);
    view! { <div class="markdown" inner_html=html /> }
}

#[component]
pub fn Avatar(#[prop(into)] name: String) -> impl IntoView {
    let initials: String = name
        .split_whitespace()
        .take(2)
        .filter_map(|word| word.chars().next())
        .collect();
    view! {
        <span class="avatar" aria-hidden="true">
            {initials.to_uppercase()}
        </span>
    }
}

#[component]
pub fn Json(#[prop(into)] value: Value) -> impl IntoView {
    view! { <pre class="json">{serde_json::to_string_pretty(&value).unwrap_or_default()}</pre> }
}

pub fn timestamp(value: &str) -> String {
    let date = js_sys::Date::new(&wasm_bindgen::JsValue::from_str(value));
    if date.get_time().is_nan() {
        return value.to_owned();
    }
    let options = js_sys::Object::new();
    for (key, value) in [
        ("month", "short"),
        ("day", "numeric"),
        ("hour", "numeric"),
        ("minute", "2-digit"),
    ] {
        let _ = js_sys::Reflect::set(&options, &key.into(), &value.into());
    }
    date.to_locale_string("en-US", &options)
        .as_string()
        .unwrap_or_default()
}
